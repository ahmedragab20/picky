//! PHP dependency extraction. PHP links files by class name (autoloading), so the
//! graph indexes every declared class and function, then resolves the names each
//! file mentions. Names are read from the raw text, comments and strings included:
//! docblock types and `'App\Models\User'` strings are real dependencies for static
//! analysis and containers, and an extra edge only ever selects more.

use std::sync::LazyLock;

use indexmap::{IndexMap, IndexSet};

use crate::jsre::{JsMatch, JsRegex, from_units, units};
use crate::npath::posix;

#[derive(Clone, Debug, Default)]
pub struct PhpFile {
    /// Lowercased fully-qualified names this file declares (classes, interfaces, traits, enums).
    pub classes: Vec<String>,
    /// Lowercased fully-qualified names of functions declared outside classes.
    pub functions: Vec<String>,
    /// Lowercased fully-qualified names this file may reference.
    pub references: IndexSet<String>,
    /// Repo-relative paths named by `require`/`include` with a literal path.
    pub includes: Vec<String>,
}

// Statements start a line, or follow `;`, a brace or the open tag: `<?php namespace App; final class Money {}`.
// TS writes the line start as `^` with the `m` flag. JavaScript's `^` also matches after `\r`, U+2028 and U+2029,
// which Rust can only say with a lookbehind, and a lookbehind needs the backtracking engine: about 3x slower than
// TS on a PHP repo. Matching the line terminator itself (group 1) finds the same statements, with the same groups;
// such a match starts one terminator early (see `statement_start`). No match ends on a terminator, so `g`
// iteration can't skip one.
const STATEMENT: &str = r"(?:^|([\n\r\u2028\u2029])|[;{}]|<\?php)\s*";

static NAME: LazyLock<JsRegex> =
    LazyLock::new(|| JsRegex::new(r"\\?[A-Za-z_\x80-\uFFFF][\w\x80-\uFFFF]*(?:\\[A-Za-z_\x80-\uFFFF][\w\x80-\uFFFF]*)*", "g"));
static NAMESPACE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(&format!(r"{STATEMENT}namespace\s+([\w\\]+)\s*[;{{]"), "g"));
static DECLARATION: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(&format!(r"{STATEMENT}(?:(?:abstract|final|readonly)\s+)*(?:class|interface|trait|enum)\s+([A-Za-z_]\w*)"), "g")
});
static FUNCTION: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(&format!(r"{STATEMENT}function\s+&?\s*([A-Za-z_]\w*)\s*\("), "g"));
static USE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(&format!(r"{STATEMENT}use\s+(function\s+|const\s+)?([^;]+);"), "g"));
static INCLUDE: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(
        // TS closes the path with a backreference to its quote, `(['"])([^'"]+\.php)\3`; one alternative per quote
        // matches the same and keeps the regex off the backtracking engine.
        r#"\b(?:require|include)(?:_once)?\s*\(?\s*(__DIR__\s*\.\s*|dirname\(\s*__DIR__\s*(?:,\s*(\d+)\s*)?\)\s*\.\s*)?(?:'([^'"]+\.php)'|"([^'"]+\.php)")"#,
        "g",
    )
});
static USE_GROUP: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^([\w\\]*)\\\{([^}]*)\}$", ""));
static USE_PART: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\\?([\w\\]+)(?:\s+as\s+(\w+))?$", ""));
static SPACES: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\s+", "g"));

fn real(u: &str) -> String {
    from_units(u).into_owned()
}

/// Where JavaScript's match of a [`STATEMENT`] regex starts: after the line terminator it matched, if any.
fn statement_start(m: &JsMatch) -> usize {
    m.get(1).map_or(m.start, |terminator| m.start + terminator.len())
}

pub fn parse_php(file: &str, text: &str) -> PhpFile {
    // Strings often spell namespaces with escaped separators: 'App\\Models\\User'.
    let text = units(text).replace("\\\\", "\\");
    let namespaces: Vec<String> = NAMESPACE
        .match_all(&text)
        .iter()
        .map(|m| {
            let ns = real(m.str(2));
            ns.strip_prefix('\\').map_or(ns.clone(), str::to_string)
        })
        .collect();
    let ns = namespaces.first().cloned().unwrap_or_default();
    let qualify = |name: &str| if ns.is_empty() { name.to_lowercase() } else { format!("{ns}\\{name}").to_lowercase() };

    let classes: Vec<String> = DECLARATION.match_all(&text).iter().map(|m| qualify(&real(m.str(2)))).collect();
    let functions: Vec<String> =
        if classes.is_empty() { FUNCTION.match_all(&text).iter().map(|m| qualify(&real(m.str(2)))).collect() } else { Vec::new() };

    // `use` at the top imports names; inside a class body it pulls in a trait (a plain reference).
    let header = match DECLARATION.exec(&text) {
        Some(m) => &text[..statement_start(&m)],
        None => &text[..],
    };
    let mut aliases: IndexMap<String, String> = IndexMap::new();
    for m in USE.match_all(header) {
        for (alias, target) in use_clause(&real(m.str(3))) {
            aliases.insert(alias.to_lowercase(), target.to_lowercase());
        }
    }

    let mut references: IndexSet<String> = aliases.values().cloned().collect();
    let tokens: IndexSet<String> = NAME.match_all(&text).iter().map(|m| real(m.str(0))).collect();
    for token in &tokens {
        for candidate in resolve_name(token, &namespaces, &aliases) {
            references.insert(candidate);
        }
    }
    for own in &classes {
        references.shift_remove(own);
    }

    PhpFile { classes, functions, references, includes: includes(file, &text) }
}

/// `A\B`, `A\B as C`, `A\{B, C as D}` -> [alias, fully-qualified name] pairs.
fn use_clause(clause: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let squeezed = units(clause);
    let squeezed = SPACES.replace_all(&squeezed, |_| " ".into());
    let squeezed = crate::js::trim(&squeezed);
    let parts: Vec<String> = match USE_GROUP.exec(squeezed) {
        Some(g) => {
            let (prefix, list) = (real(g.str(1)), real(g.str(2)));
            list.split(',').map(|p| format!("{prefix}\\{}", crate::js::trim(p))).collect()
        }
        None => clause.split(',').map(str::to_string).collect(),
    };
    for part in parts {
        let part = units(crate::js::trim(&part)).into_owned();
        let Some(m) = USE_PART.exec(&part) else { continue };
        let name = real(m.str(1));
        let alias = m.get(2).map(real).unwrap_or_else(|| name.rsplit('\\').next().unwrap_or("").to_string());
        out.push((alias, name));
    }
    out
}

fn resolve_name(token: &str, namespaces: &[String], aliases: &IndexMap<String, String>) -> Vec<String> {
    if let Some(rest) = token.strip_prefix('\\') {
        return vec![rest.to_lowercase()];
    }
    let lower = token.to_lowercase();
    let mut parts = lower.split('\\');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let mut out = Vec::new();
    let alias = aliases.get(first).filter(|a| !a.is_empty());
    if let Some(alias) = alias {
        let mut joined = vec![alias.as_str()];
        joined.extend(&rest);
        out.push(joined.join("\\"));
    }
    for ns in namespaces {
        out.push(format!("{}\\{lower}", ns.to_lowercase()));
    }
    // Global names: functions and constants fall back to them, and strings hold fully-qualified names.
    if !rest.is_empty() || namespaces.is_empty() || alias.is_none() {
        out.push(lower.clone());
    }
    out
}

fn includes(file: &str, text: &str) -> Vec<String> {
    let dir = posix::dirname(file);
    let mut out = Vec::new();
    for m in INCLUDE.match_all(text) {
        let prefix = m.get(1);
        let target = real(m.get(3).or(m.get(4)).unwrap_or(""));
        let mut base = dir.clone();
        if prefix.is_some_and(|p| p.starts_with("dirname")) {
            let levels = m.get(2).map_or(1.0, crate::js::parse_number);
            let mut i = 0.0;
            while i < levels {
                let up = posix::dirname(&base);
                if up == base {
                    break; // `.` and `/` are their own parents
                }
                base = up;
                i += 1.0;
            }
        }
        // Without __DIR__, PHP tries the include path (usually the working directory), then the file's directory.
        let candidates = if prefix.is_some() {
            vec![posix::join(&[&base, &target])]
        } else {
            vec![posix::join(&[&dir, &target]), posix::normalize(&target)]
        };
        for c in candidates {
            if !c.starts_with("..") {
                out.push(c.strip_prefix("./").map_or(c.clone(), str::to_string));
            }
        }
    }
    out
}

/// Declared class and function names of a file's old content, to find who used a deleted file.
pub fn declared_names(text: &str) -> Vec<String> {
    let parsed = parse_php("x.php", text);
    [parsed.classes, parsed.functions].concat()
}
