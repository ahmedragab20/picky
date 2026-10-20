//! The bash hook's side: finding and rewriting `# picky` lines in a step script.

use crate::job::is_setup_command;
use crate::js;
use crate::sh::{quote, tokenize};

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptLine {
    /// 1-based.
    pub line: usize,
    pub text: String,
    pub marked: bool,
    /// False for heredocs and continued lines, which picky never rewrites.
    pub simple: bool,
}

/// `(^|\s)#\s*picky(\s|$)`
pub fn has_marker(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    (0..chars.len()).any(|i| {
        if chars[i] != '#' || (i > 0 && !js::is_space(chars[i - 1])) {
            return false;
        }
        let mut j = i + 1;
        while j < chars.len() && js::is_space(chars[j]) {
            j += 1;
        }
        let word: String = chars[j..chars.len().min(j + 5)].iter().collect();
        word == "picky" && chars.get(j + 5).is_none_or(|c| js::is_space(*c))
    })
}

/// `^\s*#\s*picky\s*$`
fn is_standalone_marker(line: &str) -> bool {
    js::trim(line).strip_prefix('#').is_some_and(|rest| js::trim(rest) == "picky")
}

/// `<<-?\s*(['"]?)([A-Za-z_][A-Za-z0-9_]*)\1`: the heredoc delimiter, if the line opens one.
fn heredoc(line: &str) -> Option<String> {
    let c: Vec<char> = line.chars().collect();
    for i in 0..c.len().saturating_sub(1) {
        if c[i] != '<' || c[i + 1] != '<' {
            continue;
        }
        let mut j = i + 2;
        if c.get(j) == Some(&'-') {
            j += 1;
        }
        while j < c.len() && js::is_space(c[j]) {
            j += 1;
        }
        let quote = c.get(j).copied().filter(|q| *q == '\'' || *q == '"');
        if quote.is_some() {
            j += 1;
        }
        let start = j;
        if !c.get(j).is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == '_') {
            continue;
        }
        while j < c.len() && (c[j].is_ascii_alphanumeric() || c[j] == '_') {
            j += 1;
        }
        if let Some(q) = quote
            && c.get(j) != Some(&q)
        {
            continue;
        }
        return Some(c[start..j].iter().collect());
    }
    None
}

/// Every command line of a step script (blank lines, comments and heredoc bodies left out).
pub fn scan_script(script: &str, mut mark_all: bool) -> Vec<ScriptLine> {
    let lines: Vec<&str> = script.split('\n').collect();
    let mut out = Vec::new();
    let mut heredoc_end: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(end) = &heredoc_end {
            if js::trim(line) == end {
                heredoc_end = None;
            }
            continue;
        }
        if is_standalone_marker(line) {
            mark_all = true;
            continue;
        }
        let trimmed = js::trim(line);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let doc = heredoc(line);
        if doc.is_some() {
            heredoc_end = doc.clone();
        }
        let continued = (i > 0 && js::trim_end(lines[i - 1]).ends_with('\\')) || trimmed.ends_with('\\');
        // A whole-step marker leaves setup commands to the job-level rule.
        let marked = has_marker(line) || (mark_all && !is_setup_command(tokenize(line).as_deref()));
        out.push(ScriptLine { line: i + 1, text: line.to_string(), marked, simple: doc.is_none() && !continued });
    }
    out
}

/// 1-based line numbers of commands picky should handle.
pub fn marked_lines(script: &str, mark_all: bool) -> Vec<usize> {
    scan_script(script, mark_all).into_iter().filter(|l| l.marked && l.simple).map(|l| l.line).collect()
}

/// 1-based line numbers of unmarked setup commands (`npm ci`, `composer install`, ...).
pub fn setup_lines(script: &str, mark_all: bool) -> Vec<usize> {
    scan_script(script, mark_all)
        .into_iter()
        .filter(|l| !l.marked && l.simple && is_setup_command(tokenize(&l.text).as_deref()))
        .map(|l| l.line)
        .collect()
}

/// Leading JavaScript whitespace (`/^\s*/`).
pub fn indent_of(line: &str) -> &str {
    &line[..line.len() - js::trim_start(line).len()]
}

/// Rewrites marked and setup lines so picky decides each command at run time. If picky
/// fails for any reason, the original line runs.
pub fn rewrite_script(script: &str, script_path: &str, picky_bin: &str, mark_all: bool) -> String {
    let mut lines: Vec<String> = script.split('\n').map(str::to_string).collect();
    let all = if mark_all { " --all" } else { "" };
    let mut targets = marked_lines(script, mark_all);
    targets.extend(setup_lines(script, mark_all));
    for n in targets {
        let original = lines[n - 1].clone();
        let indent = indent_of(&original);
        lines[n - 1] = format!(
            "{indent}__picky_cmd=\"$({} rewrite --script {} --line {n}{all})\" || __picky_cmd={}; eval \"$__picky_cmd\"",
            quote(picky_bin),
            quote(script_path),
            quote(js::trim(&original)),
        );
    }
    lines.join("\n")
}

/// bash flags to reproduce the step's shell options (`$-` plus pipefail).
pub fn bash_flags(shell_flags: Option<&str>, pipefail: bool) -> Vec<String> {
    let mut flags = vec!["--noprofile".to_string(), "--norc".to_string()];
    for f in ['e', 'u', 'x'] {
        if shell_flags.is_some_and(|s| s.contains(f)) {
            flags.push(format!("-{f}"));
        }
    }
    if pipefail {
        flags.push("-o".into());
        flags.push("pipefail".into());
    }
    flags
}
