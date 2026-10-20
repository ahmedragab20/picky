//! A std-only port of eemeli/yaml 2.9.1 `parseDocument` (lexer, CST parser and
//! composer, YAML 1.2 core schema, default options).
//!
//! The port follows the JavaScript sources function by function so that error
//! recovery, ranges, comments and error messages match. Offsets in the public
//! API are BYTE offsets into the source `&str`; the JavaScript library uses
//! UTF-16 indices. Messages (which contain line and column numbers) are
//! computed with UTF-16 columns exactly like the library does.
//!
//! Not ported: stringify, non-core schemas (`%YAML 1.1`, `!!set`, `!!omap`,
//! `!!pairs`, `!!binary`, `!!timestamp`, `!!merge`), custom tags, merge keys,
//! the visitor API and every option other than the defaults.
//!
//! Known differences from the JavaScript library:
//! - `%YAML 1.1` documents are read with the core schema (JS switches to the
//!   yaml-1.1 schema); YAML 1.1 tags (`!!set`, `!!omap`, `!!pairs`, `!!binary`,
//!   `!!timestamp`, `!!merge`) are treated like unknown tags (JS resolves them to
//!   `Set`, `Map`, `Uint8Array`, `Date`).
//! - `toJS()` of a document with an alias that refers to a node containing it
//!   (`&a [*a]`) is an `Err`: JS returns a cyclic object, which a [`Value`] cannot hold.
//! - A collection used as a map key is converted to a key string by an
//!   approximation of the flow-style stringifier (no comments, no line folding, the
//!   original text of null/bool/number scalars is not known).
//! - Lone UTF-16 surrogates cannot exist in a Rust `String` (a `"\uD83D"` escape not
//!   followed by a low surrogate, or an astral character cut in half in an error
//!   message, becomes U+FFFD or is dropped).
//! - Positions are byte offsets. Where the JS code computes a position as
//!   `offset + source.length` from an `offset` that is not where the token really
//!   starts (only in malformed documents), the result differs by the byte/UTF-16
//!   length difference of non-ASCII text.
//! - Nesting deeper than 1500 levels is reported as `RESOURCE_EXHAUSTION` (JS fails
//!   at roughly 1600 levels, depending on its stack); deeper input is composed on a
//!   thread with a bigger stack instead of overflowing the caller's.

#![allow(dead_code)] // mirrors a library API; callers use a subset

use std::collections::{HashMap, HashSet};
use std::fmt;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// The JS value produced by `doc.toJS()` (the YAML 1.2 core schema only
/// produces these shapes; `undefined` is unreachable).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    /// Own enumerable properties in JS enumeration order.
    Object(Vec<(String, Value)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarStyle {
    Plain,
    QuoteDouble,
    QuoteSingle,
    BlockLiteral,
    BlockFolded,
}

/// A parse error or warning (`YAMLParseError` / `YAMLWarning`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlError {
    /// eemeli `ErrorCode`, e.g. `DUPLICATE_KEY`.
    pub code: String,
    /// `.message`, including the pretty-errors ` at line L, column C:\n\n...` suffix.
    pub message: String,
    /// Byte range `[start, end)` in the source.
    pub pos: (usize, usize),
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for YamlError {}

#[derive(Clone, Debug, PartialEq)]
pub struct Pair {
    pub key: Option<Node>,
    pub value: Option<Node>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Scalar {
        value: Value,
        style: ScalarStyle,
    },
    /// A flow sequence item of the form `a: b` is a single-pair `Map` with `flow: true`
    /// (this is how eemeli models it).
    Map {
        items: Vec<Pair>,
        flow: bool,
    },
    Seq {
        items: Vec<Node>,
        flow: bool,
    },
    Alias {
        source: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    /// `[value start, value end, node end (incl. trailing comments/whitespace)]`.
    pub range: [usize; 3],
    pub anchor: Option<String>,
    pub tag: Option<String>,
    pub comment: Option<String>,
    pub comment_before: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub contents: Option<Node>,
    pub errors: Vec<YamlError>,
    pub warnings: Vec<YamlError>,
    /// `doc.commentBefore` (extension to the requested shape).
    pub comment_before: Option<String>,
    /// `doc.comment` (extension to the requested shape).
    pub comment: Option<String>,
    /// `doc.range` (extension to the requested shape).
    pub range: [usize; 3],
}

/// Mirrors `YAML.parseDocument(src)` with default options.
pub fn parse_document(src: &str) -> Document {
    let lexemes = Lexer::new(src).lex();
    let mut parser = Parser::new(src);
    parser.parse(&lexemes);
    drop(lexemes);
    let Parser { out, out_lines, line_starts, max_stack, .. } = parser;

    let all_lines = line_starts.len();
    // Composing recurses once per nesting level; deeply nested input runs on a
    // thread with a stack sized for it instead of overflowing the caller's.
    let composed = with_stack_for(max_stack, |max_depth| compose_tokens(src, &out, &out_lines, all_lines, max_depth));
    dispose_tokens(out);
    let (mut doc, known_lines) = composed;
    let lc = LineCounter { line_starts: &line_starts[..known_lines.min(line_starts.len())] };
    for e in doc.errors.iter_mut().chain(doc.warnings.iter_mut()) {
        prettify_error(src, &lc, e);
    }
    doc
}

/// The loop of `parseDocument()` over `composer.compose(tokens, true, src.length)`;
/// returns the document and the number of line starts the JS line counter would know.
fn compose_tokens(src: &str, out: &[Token], out_lines: &[usize], all_lines: usize, max_depth: usize) -> (Document, usize) {
    let mut composer = Composer::new(src, max_depth);
    let mut first: Option<Document> = None;
    let mut known_lines = all_lines;
    let mut second: Option<Document> = None;
    for (token, lines) in out.iter().zip(out_lines.iter()) {
        if let Some(d) = composer.next(token) {
            if first.is_none() {
                first = Some(d);
            } else {
                // the consumer in parseDocument() stops iterating here, so the
                // line counter only knows the lines seen up to this token
                known_lines = *lines;
                second = Some(d);
                break;
            }
        }
    }
    if second.is_none()
        && let Some(d) = composer.end(true, src.len())
    {
        if first.is_none() {
            first = Some(d);
        } else {
            second = Some(d);
        }
    }
    if let Some(d) = &second
        && let Some(doc) = first.as_mut()
    {
        doc.errors.push(YamlError {
            code: "MULTIPLE_DOCS".to_string(),
            message: "Source contains multiple documents; please use YAML.parseAllDocuments()".to_string(),
            pos: (d.range[0], d.range[1]),
        });
    }
    let doc = first.unwrap_or_else(|| Document {
        contents: None,
        errors: Vec::new(),
        warnings: Vec::new(),
        comment_before: None,
        comment: None,
        range: [0, src.len(), src.len()],
    });
    (doc, known_lines)
}

/// Nesting depth up to which work runs on the caller's stack.
const INLINE_DEPTH: usize = 40;
/// Nesting limit when no extra stack could be allocated.
const FALLBACK_DEPTH: usize = 64;

/// Runs `f(max_depth)`. When `depth` (a nesting estimate) is large, `f` runs on a
/// scoped thread with a stack big enough for `MAX_DEPTH` levels; if such a thread
/// cannot be created `f` runs inline with a reduced depth limit.
fn with_stack_for<R: Send>(depth: usize, f: impl FnOnce(usize) -> R + Send) -> R {
    if depth <= INLINE_DEPTH {
        return f(MAX_DEPTH);
    }
    // ~13 KB per level in debug builds, ~3 KB in release builds
    let bytes = (depth.min(MAX_DEPTH + 8) * 32 * 1024).max(1 << 20) + (1 << 20);
    let f = std::sync::Mutex::new(Some(f));
    let run = |limit: usize| -> Option<R> {
        let f = f.lock().ok()?.take()?;
        Some(f(limit))
    };
    let threaded = std::thread::scope(|scope| {
        std::thread::Builder::new().stack_size(bytes).spawn_scoped(scope, || run(MAX_DEPTH)).ok().and_then(|h| h.join().ok()).flatten()
    });
    match threaded {
        Some(r) => r,
        None => run(FALLBACK_DEPTH).expect("closure is only consumed once"),
    }
}

/// Mirrors `YAML.parse(src)` with default options: `Err(first error)` when the
/// document has errors, otherwise `doc.toJS()`. Warnings are ignored. An error
/// thrown by `toJS()` (e.g. an unresolved alias) is returned as a `YamlError`
/// with the code `REFERENCE_ERROR` (not an eemeli code) and an empty position.
pub fn parse(src: &str) -> Result<Value, YamlError> {
    let doc = parse_document(src);
    if let Some(e) = doc.errors.first() {
        return Err(e.clone());
    }
    doc.to_js().map_err(|message| YamlError { code: "REFERENCE_ERROR".to_string(), message, pos: (0, 0) })
}

impl Document {
    /// `doc.toJS()`; `Err` carries the message of the error `toJS` would throw
    /// (`Unresolved alias ...`, `Excessive alias count ...`) or, as a deviation, the
    /// message `Circular alias reference ...` (see the module docs).
    pub fn to_js(&self) -> Result<Value, String> {
        let Some(root) = &self.contents else {
            return Ok(Value::Null);
        };
        with_stack_for(node_depth(root), |_| {
            let mut cx = ToJs::new(root);
            let res = cx.convert(root)?;
            if cx.circular {
                return Err(CIRCULAR_MSG.to_string());
            }
            Ok(res)
        })
    }

    /// `doc.get(key, true)`: the value node of the first pair of a root map whose
    /// key is the string scalar `key` (for a root seq, the item at index `Number(key)`).
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.contents.as_ref().and_then(|c| c.get(key))
    }

    /// `doc.getIn(path, true)`; an empty path returns the contents.
    pub fn get_in(&self, path: &[&str]) -> Option<&Node> {
        if path.is_empty() {
            return self.contents.as_ref();
        }
        let mut node = self.contents.as_ref()?;
        for (i, key) in path.iter().enumerate() {
            let next = node.get(key)?;
            if i + 1 == path.len() {
                return Some(next);
            }
            node = next;
        }
        None
    }
}

impl Node {
    /// `YAMLMap.get(key, true)` for maps; `YAMLSeq.get(key, true)` (the key is
    /// converted with `Number(key)`) for sequences; `None` for scalars and aliases.
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.kind {
            NodeKind::Map { items, .. } => {
                for p in items {
                    if let Some(Node { kind: NodeKind::Scalar { value, .. }, .. }) = &p.key
                        && let Value::String(s) = value
                        && s == key
                    {
                        return p.value.as_ref();
                    }
                }
                None
            }
            NodeKind::Seq { items, .. } => {
                let idx = as_item_index(key)?;
                items.get(idx)
            }
            _ => None,
        }
    }
}

/// `asItemIndex(key)` for a string key.
fn as_item_index(key: &str) -> Option<usize> {
    if key.is_empty() {
        return None;
    }
    let n = js_string_to_number(key);
    if n.is_finite() && n.fract() == 0.0 && n >= 0.0 && n < usize::MAX as f64 { Some(n as usize) } else { None }
}

// ---------------------------------------------------------------------------
// JS helpers
// ---------------------------------------------------------------------------

fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// `String.prototype.trim`
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// `Number(str)` for the cases that matter here.
fn js_string_to_number(s: &str) -> f64 {
    let t = js_trim(s);
    if t.is_empty() {
        return 0.0;
    }
    let b = t.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            return parse_radix(&t[2..], radix).unwrap_or(f64::NAN);
        }
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // StrDecimalLiteral: digits, optional fraction, optional exponent
    let mut i = 0;
    if matches!(b[0], b'+' | b'-') {
        i = 1;
    }
    let mut digits = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        i += 1;
        if i < b.len() && matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        let mut ed = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            ed += 1;
        }
        if ed == 0 {
            return f64::NAN;
        }
    }
    if i != b.len() {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// Exact (round-to-nearest-even) value of a radix 2, 8 or 16 digit string and,
/// for radix 10, of a decimal digit string. `None` if a digit is invalid or
/// there are no digits.
fn parse_radix(digits: &str, radix: u32) -> Option<f64> {
    if digits.is_empty() {
        return None;
    }
    let bits_per = match radix {
        2 => 1,
        8 => 3,
        16 => 4,
        10 => {
            return if digits.bytes().all(|c| c.is_ascii_digit()) { digits.parse::<f64>().ok() } else { None };
        }
        _ => return None,
    };
    let mut vals: Vec<u8> = Vec::with_capacity(digits.len());
    for c in digits.chars() {
        vals.push(c.to_digit(radix)? as u8);
    }
    // strip leading zeros
    let first = vals.iter().position(|&v| v != 0);
    let Some(first) = first else {
        return Some(0.0);
    };
    let vals = &vals[first..];
    let top_bits = 32 - u32::from(vals[0]).leading_zeros();
    let total_bits = (vals.len() as u64 - 1) * bits_per as u64 + top_bits as u64;
    if total_bits <= 64 {
        let mut acc: u64 = 0;
        for &v in vals {
            acc = (acc << bits_per) | u64::from(v);
        }
        return Some(acc as f64);
    }
    // take the top 64 bits, fold the rest into a sticky bit
    let mut acc: u64 = 0;
    let mut taken: u64 = 0;
    let mut sticky = false;
    let mut shift_out: u64 = 0;
    for (idx, &v) in vals.iter().enumerate() {
        let bits_here = if idx == 0 { top_bits as u64 } else { bits_per as u64 };
        for b in (0..bits_here).rev() {
            let bit = (u64::from(v) >> b) & 1;
            if taken < 64 {
                acc = (acc << 1) | bit;
                taken += 1;
            } else {
                shift_out += 1;
                if bit == 1 {
                    sticky = true;
                }
            }
        }
    }
    if sticky {
        acc |= 1;
    }
    let mut f = acc as f64;
    let mut remaining = shift_out;
    while remaining > 0 {
        let step = remaining.min(1000);
        f *= 2f64.powi(step as i32);
        remaining -= step;
        if f.is_infinite() {
            break;
        }
    }
    Some(f)
}

/// `String(number)` / `Number.prototype.toString()`
pub fn js_number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x == 0.0 {
        return "0".to_string();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    let neg = x < 0.0;
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let e = n - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&e.abs().to_string());
    }
    out
}

/// `JSON.stringify(string)`
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `decodeURIComponent`; `Err` is the `URIError`.
fn js_decode_uri_component(s: &str) -> Result<String, ()> {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            let start = i;
            while i < b.len() && b[i] != b'%' {
                i += 1;
            }
            out.push_str(&s[start..i]);
            continue;
        }
        let mut bytes = Vec::new();
        while i < b.len() && b[i] == b'%' {
            let h = b.get(i + 1).and_then(|c| (*c as char).to_digit(16));
            let l = b.get(i + 2).and_then(|c| (*c as char).to_digit(16));
            match (h, l) {
                (Some(h), Some(l)) => bytes.push((h * 16 + l) as u8),
                _ => return Err(()),
            }
            i += 3;
        }
        out.push_str(std::str::from_utf8(&bytes).map_err(|_| ())?);
    }
    Ok(out)
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Byte index reached after advancing `n` UTF-16 units from `start`.
fn advance_units(s: &str, start: usize, n: usize) -> usize {
    let mut i = start.min(s.len());
    let mut left = n;
    while left > 0 && i < s.len() {
        let c = s[i..].chars().next().expect("char");
        i += c.len_utf8();
        left = left.saturating_sub(c.len_utf16());
    }
    i
}

// ---------------------------------------------------------------------------
// Pretty errors (errors.js prettifyError, parse/line-counter.js)
// ---------------------------------------------------------------------------

struct LineCounter<'a> {
    line_starts: &'a [usize],
}

impl LineCounter<'_> {
    /// Returns the 1-indexed (line, byte offset of that line start or None).
    /// `line == 0` mirrors the JS `{ line: 0, col: offset }` result.
    fn line_pos(&self, offset: usize) -> (usize, Option<usize>) {
        let (mut low, mut high) = (0usize, self.line_starts.len());
        while low < high {
            let mid = (low + high) >> 1;
            if self.line_starts[mid] < offset {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        if self.line_starts.get(low) == Some(&offset) {
            return (low + 1, Some(offset));
        }
        if low == 0 {
            return (0, None);
        }
        (low, Some(self.line_starts[low - 1]))
    }
}

/// UTF-16 units between byte offsets `from` and `to`; a `to` inside a char counts
/// `min(bytes into the char, units of the char)` so that `offset + 1` is one unit.
fn units_between(src: &str, from: usize, to: usize) -> usize {
    let from = from.min(src.len());
    let to = to.min(src.len());
    if to <= from {
        return 0;
    }
    let mut n = 0;
    let mut i = from;
    while i < to {
        let Some(c) = src.get(i..).and_then(|s| s.chars().next()) else {
            // not on a char boundary: step to the next boundary
            i += 1;
            n += 1;
            continue;
        };
        let l = c.len_utf8();
        if i + l <= to {
            n += c.len_utf16();
        } else {
            n += (to - i).min(c.len_utf16());
        }
        i += l;
    }
    n
}

fn prettify_error(src: &str, lc: &LineCounter<'_>, error: &mut YamlError) {
    let (line0, start0) = lc.line_pos(error.pos.0);
    let (line1, start1) = lc.line_pos(error.pos.1);
    let col0 = match start0 {
        Some(s) => units_between(src, s, error.pos.0) + 1,
        None => error.pos.0,
    };
    let col1 = match start1 {
        Some(s) => units_between(src, s, error.pos.1) + 1,
        None => error.pos.1,
    };
    let (line, col) = (line0, col0);
    error.message.push_str(&format!(" at line {line}, column {col}"));
    let mut ci = col as isize - 1;
    let ls = |i: isize| -> usize { if i < 0 { 0 } else { lc.line_starts.get(i as usize).copied().unwrap_or(src.len()) } };
    let slice_units = |a: usize, b: usize| -> Vec<u16> {
        let a = a.min(src.len());
        let b = b.min(src.len()).max(a);
        src.get(a..b).map(|s| s.encode_utf16().collect()).unwrap_or_default()
    };
    let line_start_idx = line as isize - 1;
    // src.substring(lineStarts[line - 1], lineStarts[line]) -- an undefined end means "to the end"
    let end_of_line = match lc.line_starts.get(line) {
        Some(e) => *e,
        None => src.len(),
    };
    let mut line_str = slice_units(ls(line_start_idx), end_of_line);
    while matches!(line_str.last(), Some(0x0a) | Some(0x0d)) {
        line_str.pop();
    }
    // Trim to max 80 chars, keeping col position near the middle
    if ci >= 60 && line_str.len() > 80 {
        let trim_start = (ci - 39).min(line_str.len() as isize - 79).max(0) as usize;
        let mut ns: Vec<u16> = vec![0x2026];
        ns.extend_from_slice(&line_str[trim_start.min(line_str.len())..]);
        line_str = ns;
        ci -= trim_start as isize - 1;
    }
    if line_str.len() > 80 {
        line_str.truncate(79);
        line_str.push(0x2026);
    }
    // Include previous line in context if pointing at line start
    if line > 1 {
        let upto = (ci.max(0) as usize).min(line_str.len());
        if line_str[..upto].iter().all(|&c| c == 0x20) {
            let mut prev = slice_units(ls(line as isize - 2), ls(line as isize - 1));
            if prev.len() > 80 {
                prev.truncate(79);
                prev.push(0x2026);
                prev.push(0x0a);
            }
            prev.extend_from_slice(&line_str);
            line_str = prev;
        }
    }
    if line_str.iter().any(|&c| c != 0x20) {
        let mut count: isize = 1;
        if line1 == line && col1 > col {
            count = ((col1 - col) as isize).min(80 - ci).max(1);
        }
        let pointer: String = " ".repeat(ci.max(0) as usize) + &"^".repeat(count as usize);
        error.message.push_str(&format!(":\n\n{}\n{}\n", String::from_utf16_lossy(&line_str), pointer));
    }
}

// ---------------------------------------------------------------------------
// Lexer (parse/lexer.js)
//
// The whole input is always available (`atEnd`), so the "incomplete input"
// paths of the JS lexer (`setNext`) are unreachable and omitted. Positions are
// `isize` because the JS code relies on `-1` sentinels and out-of-range reads
// returning `undefined` (`None` here).
// ---------------------------------------------------------------------------

/// One lexeme: either a control character or a slice of the input.
#[derive(Clone, Copy, Debug)]
enum Lex {
    /// `\x02` a document starts with the next token
    Doc,
    /// `\x18` unexpected end of flow-mode
    FlowEnd,
    /// `\x1f` next token is a scalar value
    Scalar,
    Text(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LState {
    Stream,
    BlockStart,
    Doc,
    Flow,
}

/// `getLine()` result: `buffer.substring(start, end)`
#[derive(Clone, Copy)]
struct Line {
    start: isize,
    end: isize,
}

impl Line {
    fn len(self) -> isize {
        self.end - self.start
    }
}

fn is_empty_ch(ch: Option<u8>) -> bool {
    matches!(ch, None | Some(b' ' | b'\n' | b'\r' | b'\t'))
}

fn is_flow_indicator(ch: Option<u8>) -> bool {
    matches!(ch, Some(b',' | b'[' | b']' | b'{' | b'}'))
}

fn is_not_anchor_char(ch: Option<u8>) -> bool {
    matches!(ch, None | Some(b' ' | b',' | b'[' | b']' | b'{' | b'}' | b'\n' | b'\r' | b'\t'))
}

fn is_tag_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            b'-' | b'#'
                | b';'
                | b'/'
                | b'?'
                | b':'
                | b'@'
                | b'&'
                | b'='
                | b'+'
                | b'$'
                | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')'
        )
}

struct Lexer<'a> {
    buf: &'a [u8],
    len: isize,
    block_scalar_indent: isize,
    block_scalar_keep: bool,
    flow_key: bool,
    flow_level: isize,
    indent_next: isize,
    indent_value: isize,
    line_end_pos: Option<isize>,
    pos: isize,
    out: Vec<Lex>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            buf: src.as_bytes(),
            len: src.len() as isize,
            block_scalar_indent: -1,
            block_scalar_keep: false,
            flow_key: false,
            flow_level: 0,
            indent_next: 0,
            indent_value: 0,
            line_end_pos: None,
            pos: 0,
            out: Vec::with_capacity(src.len() / 4 + 8),
        }
    }

    fn lex(mut self) -> Vec<Lex> {
        let mut next = LState::Stream;
        while self.pos < self.len {
            next = match next {
                LState::Stream => self.parse_stream(),
                LState::BlockStart => self.parse_block_start(),
                LState::Doc => self.parse_document(),
                LState::Flow => self.parse_flow_collection(),
            };
        }
        self.out
    }

    fn ch(&self, i: isize) -> Option<u8> {
        if i < 0 || i >= self.len { None } else { Some(self.buf[i as usize]) }
    }

    fn char_at(&self, n: isize) -> Option<u8> {
        self.ch(self.pos + n)
    }

    /// `buffer[pos + 1]` where JS indexes UTF-16 units: the byte after the current
    /// character (an astral character's second unit is never whitespace).
    fn char_at_unit1(&self) -> Option<u8> {
        match self.char_at(0) {
            Some(b) if b >= 0xf0 => Some(b),
            Some(b) if b >= 0xc0 => {
                let n = if b >= 0xe0 { 3 } else { 2 };
                self.char_at(n)
            }
            _ => self.char_at(1),
        }
    }

    /// `line[n]`
    fn lat(&self, line: Line, n: isize) -> Option<u8> {
        if n < 0 || n >= line.len() { None } else { self.ch(line.start + n) }
    }

    fn starts_with_at(&self, i: isize, pat: &[u8]) -> bool {
        if i < 0 {
            return false;
        }
        self.buf.get(i as usize..).is_some_and(|rest| rest.starts_with(pat))
    }

    /// `buffer.indexOf(byte, from)`
    fn index_of(&self, b: u8, from: isize) -> isize {
        let from = from.max(0);
        if from >= self.len {
            return -1;
        }
        match self.buf[from as usize..].iter().position(|&x| x == b) {
            Some(p) => from + p as isize,
            None => -1,
        }
    }

    fn at_line_end(&self) -> bool {
        let mut i = self.pos;
        let mut ch = self.ch(i);
        while ch == Some(b' ') || ch == Some(b'\t') {
            i += 1;
            ch = self.ch(i);
        }
        match ch {
            None | Some(b'#' | b'\n') => true,
            Some(b'\r') => self.ch(i + 1) == Some(b'\n'),
            _ => false,
        }
    }

    fn continue_scalar(&self, offset: isize) -> isize {
        let mut ch = self.ch(offset);
        if self.indent_next > 0 {
            let mut indent: isize = 0;
            while ch == Some(b' ') {
                indent += 1;
                ch = self.ch(indent + offset);
            }
            if ch == Some(b'\r') && self.ch(indent + offset + 1) == Some(b'\n') {
                return offset + indent + 1;
            }
            return if ch == Some(b'\n') || indent >= self.indent_next { offset + indent } else { -1 };
        }
        if (ch == Some(b'-') || ch == Some(b'.'))
            && (self.starts_with_at(offset, b"---") || self.starts_with_at(offset, b"..."))
            && is_empty_ch(self.ch(offset + 3))
        {
            return -1;
        }
        offset
    }

    fn get_line(&mut self) -> Line {
        let need = match self.line_end_pos {
            None => true,
            Some(e) => e != -1 && e < self.pos,
        };
        let end = if need {
            let e = self.index_of(b'\n', self.pos);
            self.line_end_pos = Some(e);
            e
        } else {
            self.line_end_pos.unwrap_or(-1)
        };
        if end == -1 {
            let s = self.pos.min(self.len);
            return Line { start: s, end: self.len };
        }
        let mut e = end;
        if self.ch(e - 1) == Some(b'\r') {
            e -= 1;
        }
        // String.prototype.substring swaps its arguments when start > end
        if self.pos <= e { Line { start: self.pos, end: e } } else { Line { start: e, end: self.pos } }
    }

    fn parse_stream(&mut self) -> LState {
        let mut line = self.get_line();
        if self.starts_with_at(line.start, "\u{feff}".as_bytes()) && line.len() >= 3 {
            // BOM is one UTF-16 unit in JS, three bytes here
            self.push_count(3);
            line = Line { start: line.start + 3, end: line.end };
        }
        if self.lat(line, 0) == Some(b'%') {
            let mut dir_end = line.len();
            let mut cs = self.line_index_of(line, b'#', 0);
            while cs != -1 {
                let ch = self.lat(line, cs - 1);
                if ch == Some(b' ') || ch == Some(b'\t') {
                    dir_end = cs - 1;
                    break;
                } else {
                    cs = self.line_index_of(line, b'#', cs + 1);
                }
            }
            loop {
                let ch = self.lat(line, dir_end - 1);
                if ch == Some(b' ') || ch == Some(b'\t') {
                    dir_end -= 1;
                } else {
                    break;
                }
            }
            let n = self.push_count(dir_end) + self.push_spaces(true);
            self.push_count(line.len() - n); // possible comment
            // NOTE: the JS calls `this.pushNewline()` here without `yield*`, which
            // is a no-op; the newline is emitted by the next 'stream' round.
            return LState::Stream;
        }
        if self.at_line_end() {
            let sp = self.push_spaces(true);
            self.push_count(line.len() - sp);
            self.push_newline();
            return LState::Stream;
        }
        self.out.push(Lex::Doc);
        self.parse_line_start()
    }

    /// `line.indexOf(ch, from)` relative to the line
    fn line_index_of(&self, line: Line, b: u8, from: isize) -> isize {
        let mut i = from.max(0);
        while i < line.len() {
            if self.ch(line.start + i) == Some(b) {
                return i;
            }
            i += 1;
        }
        -1
    }

    fn parse_line_start(&mut self) -> LState {
        let ch = self.char_at(0);
        if ch == Some(b'-') || ch == Some(b'.') {
            let s3 = self.starts_with_at(self.pos, b"---");
            let d3 = self.starts_with_at(self.pos, b"...");
            if (s3 || d3) && is_empty_ch(self.char_at(3)) {
                self.push_count(3);
                self.indent_value = 0;
                self.indent_next = 0;
                return if s3 { LState::Doc } else { LState::Stream };
            }
        }
        self.indent_value = self.push_spaces(false);
        if self.indent_next > self.indent_value && !is_empty_ch(self.char_at_unit1()) {
            self.indent_next = self.indent_value;
        }
        self.parse_block_start()
    }

    fn parse_block_start(&mut self) -> LState {
        let ch0 = self.char_at(0);
        let ch1 = self.char_at(1);
        if matches!(ch0, Some(b'-' | b'?' | b':')) && is_empty_ch(ch1) {
            let n = self.push_count(1) + self.push_spaces(true);
            self.indent_next = self.indent_value + 1;
            self.indent_value += n;
            return LState::BlockStart;
        }
        LState::Doc
    }

    fn parse_document(&mut self) -> LState {
        self.push_spaces(true);
        let line = self.get_line();
        let mut n = self.push_indicators();
        match self.lat(line, n) {
            Some(b'#') => {
                self.push_count(line.len() - n);
                self.push_newline();
                self.parse_line_start()
            }
            None => {
                self.push_newline();
                self.parse_line_start()
            }
            Some(b'{' | b'[') => {
                self.push_count(1);
                self.flow_key = false;
                self.flow_level = 1;
                LState::Flow
            }
            Some(b'}' | b']') => {
                // this is an error
                self.push_count(1);
                LState::Doc
            }
            Some(b'*') => {
                self.push_until(is_not_anchor_char);
                LState::Doc
            }
            Some(b'"' | b'\'') => self.parse_quoted_scalar(),
            Some(b'|' | b'>') => {
                n += self.parse_block_scalar_header();
                n += self.push_spaces(true);
                self.push_count(line.len() - n);
                self.push_newline();
                self.parse_block_scalar()
            }
            _ => self.parse_plain_scalar(),
        }
    }

    fn parse_flow_collection(&mut self) -> LState {
        let mut indent: isize = -1;
        loop {
            let nl = self.push_newline();
            let mut sp;
            if nl > 0 {
                sp = self.push_spaces(false);
                indent = sp;
                self.indent_value = sp;
            } else {
                sp = 0;
            }
            sp += self.push_spaces(true);
            if nl + sp <= 0 {
                break;
            }
        }
        let line = self.get_line();
        let l0 = self.lat(line, 0);
        if (indent != -1 && indent < self.indent_next && l0 != Some(b'#'))
            || (indent == 0 && (self.lstarts_with(line, b"---") || self.lstarts_with(line, b"...")) && is_empty_ch(self.lat(line, 3)))
        {
            // Allowing for the terminal ] or } at the same (rather than greater)
            // indent level as the initial [ or { is technically invalid, but
            // failing here would be surprising to users.
            let at_flow_end_marker = indent == self.indent_next - 1 && self.flow_level == 1 && (l0 == Some(b']') || l0 == Some(b'}'));
            if !at_flow_end_marker {
                // this is an error
                self.flow_level = 0;
                self.out.push(Lex::FlowEnd);
                return self.parse_line_start();
            }
        }
        let mut n: isize = 0;
        while self.lat(line, n) == Some(b',') {
            n += self.push_count(1);
            n += self.push_spaces(true);
            self.flow_key = false;
        }
        n += self.push_indicators();
        match self.lat(line, n) {
            None => LState::Flow,
            Some(b'#') => {
                self.push_count(line.len() - n);
                LState::Flow
            }
            Some(b'{' | b'[') => {
                self.push_count(1);
                self.flow_key = false;
                self.flow_level += 1;
                LState::Flow
            }
            Some(b'}' | b']') => {
                self.push_count(1);
                self.flow_key = true;
                self.flow_level -= 1;
                if self.flow_level != 0 { LState::Flow } else { LState::Doc }
            }
            Some(b'*') => {
                self.push_until(is_not_anchor_char);
                LState::Flow
            }
            Some(b'"' | b'\'') => {
                self.flow_key = true;
                self.parse_quoted_scalar()
            }
            Some(b':') => {
                let next = self.char_at(1);
                if self.flow_key || is_empty_ch(next) || next == Some(b',') {
                    self.flow_key = false;
                    self.push_count(1);
                    self.push_spaces(true);
                    return LState::Flow;
                }
                self.flow_key = false;
                self.parse_plain_scalar()
            }
            _ => {
                self.flow_key = false;
                self.parse_plain_scalar()
            }
        }
    }

    fn lstarts_with(&self, line: Line, pat: &[u8]) -> bool {
        line.len() >= pat.len() as isize && self.starts_with_at(line.start, pat)
    }

    fn parse_quoted_scalar(&mut self) -> LState {
        let quote = self.char_at(0).unwrap_or(b'"');
        let mut end = self.index_of(quote, self.pos + 1);
        if quote == b'\'' {
            while end != -1 && self.ch(end + 1) == Some(b'\'') {
                end = self.index_of(b'\'', end + 2);
            }
        } else {
            // double-quote
            while end != -1 {
                let mut n = 0;
                while self.ch(end - 1 - n) == Some(b'\\') {
                    n += 1;
                }
                if n % 2 == 0 {
                    break;
                }
                end = self.index_of(b'"', end + 1);
            }
        }
        // Only looking for newlines within the quotes: `qb = buffer.substring(0, end)`
        // (an `end` of -1 gives an empty string)
        let qb_end = end.max(0);
        let qb_index_of = |lx: &Lexer<'_>, from: isize| -> isize {
            let from = from.max(0);
            if from >= qb_end {
                return -1;
            }
            match lx.buf[from as usize..qb_end as usize].iter().position(|&x| x == b'\n') {
                Some(p) => from + p as isize,
                None => -1,
            }
        };
        let mut nl = qb_index_of(self, self.pos);
        if nl != -1 {
            while nl != -1 {
                let cs = self.continue_scalar(nl + 1);
                if cs == -1 {
                    break;
                }
                nl = qb_index_of(self, cs);
            }
            if nl != -1 {
                // this is an error caused by an unexpected unindent
                end = nl - if self.ch(nl - 1) == Some(b'\r') { 2 } else { 1 };
            }
        }
        if end == -1 {
            end = self.len;
        }
        self.push_to_index(end + 1, false);
        if self.flow_level != 0 { LState::Flow } else { LState::Doc }
    }

    fn parse_block_scalar_header(&mut self) -> isize {
        self.block_scalar_indent = -1;
        self.block_scalar_keep = false;
        let mut i = self.pos;
        loop {
            i += 1;
            let ch = self.ch(i);
            match ch {
                Some(b'+') => self.block_scalar_keep = true,
                Some(c @ b'1'..=b'9') => self.block_scalar_indent = isize::from(c - b'0') - 1,
                Some(b'-') => {}
                _ => break,
            }
        }
        self.push_until(|ch| is_empty_ch(ch) || ch == Some(b'#'))
    }

    fn parse_block_scalar(&mut self) -> LState {
        let mut nl = self.pos - 1; // may be -1 if this.pos === 0
        let mut indent: isize = 0;
        let mut i = self.pos;
        while let Some(ch) = self.ch(i) {
            match ch {
                b' ' => indent += 1,
                b'\n' => {
                    nl = i;
                    indent = 0;
                }
                b'\r' => {
                    if self.ch(i + 1) != Some(b'\n') {
                        break;
                    }
                }
                _ => break,
            }
            i += 1;
        }
        if indent >= self.indent_next {
            if self.block_scalar_indent == -1 {
                self.indent_next = indent;
            } else {
                self.indent_next = self.block_scalar_indent + if self.indent_next == 0 { 1 } else { self.indent_next };
            }
            loop {
                let cs = self.continue_scalar(nl + 1);
                if cs == -1 {
                    break;
                }
                nl = self.index_of(b'\n', cs);
                if nl == -1 {
                    break;
                }
            }
            if nl == -1 {
                nl = self.len;
            }
        }
        // Trailing insufficiently indented tabs are invalid.
        // To catch that during parsing, we include them in the block scalar value.
        let mut i = nl + 1;
        let mut ch = self.ch(i);
        while ch == Some(b' ') {
            i += 1;
            ch = self.ch(i);
        }
        if ch == Some(b'\t') {
            while matches!(ch, Some(b'\t' | b' ' | b'\r' | b'\n')) {
                i += 1;
                ch = self.ch(i);
            }
            nl = i - 1;
        } else if !self.block_scalar_keep {
            loop {
                let mut i = nl - 1;
                let mut ch = self.ch(i);
                if ch == Some(b'\r') {
                    i -= 1;
                    ch = self.ch(i);
                }
                let last_char = i; // Drop the line if last char not more indented
                while ch == Some(b' ') {
                    i -= 1;
                    ch = self.ch(i);
                }
                if ch == Some(b'\n') && i >= self.pos && i + 1 + indent > last_char {
                    nl = i;
                } else {
                    break;
                }
            }
        }
        self.out.push(Lex::Scalar);
        self.push_to_index(nl + 1, true);
        self.parse_line_start()
    }

    fn parse_plain_scalar(&mut self) -> LState {
        let in_flow = self.flow_level > 0;
        let mut end = self.pos - 1;
        let mut i = self.pos - 1;
        loop {
            i += 1;
            let Some(mut ch) = self.ch(i) else { break };
            if ch == b':' {
                let next = self.ch(i + 1);
                if is_empty_ch(next) || (in_flow && is_flow_indicator(next)) {
                    break;
                }
                end = i;
            } else if is_empty_ch(Some(ch)) {
                let mut next = self.ch(i + 1);
                if ch == b'\r' {
                    if next == Some(b'\n') {
                        i += 1;
                        ch = b'\n';
                        next = self.ch(i + 1);
                    } else {
                        end = i;
                    }
                }
                if next == Some(b'#') || (in_flow && is_flow_indicator(next)) {
                    break;
                }
                if ch == b'\n' {
                    let cs = self.continue_scalar(i + 1);
                    if cs == -1 {
                        break;
                    }
                    i = i.max(cs - 2); // to advance, but still account for ' #'
                }
            } else {
                if in_flow && is_flow_indicator(Some(ch)) {
                    break;
                }
                end = i;
            }
        }
        self.out.push(Lex::Scalar);
        self.push_to_index(end + 1, true);
        if in_flow { LState::Flow } else { LState::Doc }
    }

    fn push_count(&mut self, n: isize) -> isize {
        if n > 0 {
            let a = self.pos.clamp(0, self.len);
            let b = (self.pos + n).clamp(a, self.len);
            self.out.push(Lex::Text(a as usize, b as usize));
            self.pos += n;
            return n;
        }
        0
    }

    fn push_to_index(&mut self, i: isize, allow_empty: bool) -> isize {
        let a = self.pos.clamp(0, self.len);
        let b = i.clamp(a, self.len);
        let n = b - a;
        if n > 0 {
            self.out.push(Lex::Text(a as usize, b as usize));
            self.pos += n;
            n
        } else {
            if allow_empty {
                self.out.push(Lex::Text(a as usize, a as usize));
            }
            0
        }
    }

    fn push_indicators(&mut self) -> isize {
        let mut n: isize = 0;
        loop {
            match self.char_at(0) {
                Some(b'!') => {
                    n += self.push_tag();
                    n += self.push_spaces(true);
                    continue;
                }
                Some(b'&') => {
                    n += self.push_until(is_not_anchor_char);
                    n += self.push_spaces(true);
                    continue;
                }
                Some(b'-' | b'?' | b':') => {
                    // this is an error (or an indicator inside flow collections)
                    let in_flow = self.flow_level > 0;
                    let ch1 = self.char_at(1);
                    if is_empty_ch(ch1) || (in_flow && is_flow_indicator(ch1)) {
                        if !in_flow {
                            self.indent_next = self.indent_value + 1;
                        } else if self.flow_key {
                            self.flow_key = false;
                        }
                        n += self.push_count(1);
                        n += self.push_spaces(true);
                        continue;
                    }
                }
                _ => {}
            }
            break;
        }
        n
    }

    fn push_tag(&mut self) -> isize {
        if self.char_at(1) == Some(b'<') {
            let mut i = self.pos + 2;
            let mut ch = self.ch(i);
            while !is_empty_ch(ch) && ch != Some(b'>') {
                i += 1;
                ch = self.ch(i);
            }
            self.push_to_index(if ch == Some(b'>') { i + 1 } else { i }, false)
        } else {
            let mut i = self.pos + 1;
            while let Some(ch) = self.ch(i) {
                if is_tag_char(ch) {
                    i += 1;
                } else if ch == b'%'
                    && self.ch(i + 1).is_some_and(|c| c.is_ascii_hexdigit())
                    && self.ch(i + 2).is_some_and(|c| c.is_ascii_hexdigit())
                {
                    i += 3;
                } else {
                    break;
                }
            }
            self.push_to_index(i, false)
        }
    }

    fn push_newline(&mut self) -> isize {
        match self.ch(self.pos) {
            Some(b'\n') => self.push_count(1),
            Some(b'\r') if self.char_at(1) == Some(b'\n') => self.push_count(2),
            _ => 0,
        }
    }

    fn push_spaces(&mut self, allow_tabs: bool) -> isize {
        let mut i = self.pos - 1;
        loop {
            i += 1;
            let ch = self.ch(i);
            if !(ch == Some(b' ') || (allow_tabs && ch == Some(b'\t'))) {
                break;
            }
        }
        let n = i - self.pos;
        if n > 0 {
            self.out.push(Lex::Text(self.pos as usize, (self.pos + n) as usize));
            self.pos = i;
        }
        n
    }

    fn push_until(&mut self, test: fn(Option<u8>) -> bool) -> isize {
        let mut i = self.pos;
        let mut ch = self.ch(i);
        while !test(ch) {
            i += 1;
            ch = self.ch(i);
        }
        self.push_to_index(i, false)
    }
}

// ---------------------------------------------------------------------------
// CST tokens (parse/cst.js)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    // lexical (source) token types
    ByteOrderMark,
    DocMode,
    FlowErrorEnd,
    Scalar,
    DocStart,
    DocEnd,
    Newline,
    SeqItemInd,
    ExplicitKeyInd,
    MapValueInd,
    FlowMapStart,
    FlowMapEnd,
    FlowSeqStart,
    FlowSeqEnd,
    Comma,
    Space,
    Comment,
    DirectiveLine,
    Alias,
    Anchor,
    Tag,
    SingleQuotedScalar,
    DoubleQuotedScalar,
    BlockScalarHeader,
    // structural token types
    Directive,
    Document,
    Error,
    BlockScalar,
    BlockMap,
    BlockSeq,
    FlowCollection,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::ByteOrderMark => "byte-order-mark",
            Ty::DocMode => "doc-mode",
            Ty::FlowErrorEnd => "flow-error-end",
            Ty::Scalar => "scalar",
            Ty::DocStart => "doc-start",
            Ty::DocEnd => "doc-end",
            Ty::Newline => "newline",
            Ty::SeqItemInd => "seq-item-ind",
            Ty::ExplicitKeyInd => "explicit-key-ind",
            Ty::MapValueInd => "map-value-ind",
            Ty::FlowMapStart => "flow-map-start",
            Ty::FlowMapEnd => "flow-map-end",
            Ty::FlowSeqStart => "flow-seq-start",
            Ty::FlowSeqEnd => "flow-seq-end",
            Ty::Comma => "comma",
            Ty::Space => "space",
            Ty::Comment => "comment",
            Ty::DirectiveLine => "directive-line",
            Ty::Alias => "alias",
            Ty::Anchor => "anchor",
            Ty::Tag => "tag",
            Ty::SingleQuotedScalar => "single-quoted-scalar",
            Ty::DoubleQuotedScalar => "double-quoted-scalar",
            Ty::BlockScalarHeader => "block-scalar-header",
            Ty::Directive => "directive",
            Ty::Document => "document",
            Ty::Error => "error",
            Ty::BlockScalar => "block-scalar",
            Ty::BlockMap => "block-map",
            Ty::BlockSeq => "block-seq",
            Ty::FlowCollection => "flow-collection",
        }
    }
}

/// `tokenType(source)`: identify the type of a lexer token.
fn token_type(source: &str) -> Option<Ty> {
    match source {
        "\u{feff}" => return Some(Ty::ByteOrderMark),
        "\x02" => return Some(Ty::DocMode),
        "\x18" => return Some(Ty::FlowErrorEnd),
        "\x1f" => return Some(Ty::Scalar),
        "---" => return Some(Ty::DocStart),
        "..." => return Some(Ty::DocEnd),
        "" | "\n" | "\r\n" => return Some(Ty::Newline),
        "-" => return Some(Ty::SeqItemInd),
        "?" => return Some(Ty::ExplicitKeyInd),
        ":" => return Some(Ty::MapValueInd),
        "{" => return Some(Ty::FlowMapStart),
        "}" => return Some(Ty::FlowMapEnd),
        "[" => return Some(Ty::FlowSeqStart),
        "]" => return Some(Ty::FlowSeqEnd),
        "," => return Some(Ty::Comma),
        _ => {}
    }
    match source.as_bytes().first()? {
        b' ' | b'\t' => Some(Ty::Space),
        b'#' => Some(Ty::Comment),
        b'%' => Some(Ty::DirectiveLine),
        b'*' => Some(Ty::Alias),
        b'&' => Some(Ty::Anchor),
        b'!' => Some(Ty::Tag),
        b'\'' => Some(Ty::SingleQuotedScalar),
        b'"' => Some(Ty::DoubleQuotedScalar),
        b'|' | b'>' => Some(Ty::BlockScalarHeader),
        _ => None,
    }
}

/// A `SourceToken`: a slice of the input (`len` bytes at `offset`).
#[derive(Clone, Copy, Debug)]
struct Src {
    ty: Ty,
    offset: usize,
    indent: usize,
    len: usize,
}

struct ErrTok {
    offset: usize,
    message: String,
    source: String,
    /// `source.length`
    len: usize,
}

struct FlowScalar {
    ty: Ty,
    offset: usize,
    indent: usize,
    len: usize,
    end: Option<Vec<Src>>,
}

struct BlockScalar {
    offset: usize,
    indent: usize,
    /// header, then spaces/comments/newlines (and error tokens)
    props: Vec<Token>,
    /// body (`source`): byte range in the input
    src_off: usize,
    src_len: usize,
}

/// A `CollectionItem`; `sep: Some(vec![])` is a present-but-empty array.
#[derive(Default)]
struct Item {
    start: Vec<Src>,
    key: Option<Box<Token>>,
    sep: Option<Vec<Src>>,
    value: Option<Box<Token>>,
    explicit_key: bool,
}

impl Item {
    fn with_start(start: Vec<Src>) -> Item {
        Item { start, ..Item::default() }
    }
}

struct BlockMap {
    offset: usize,
    indent: usize,
    items: Vec<Item>,
}

struct BlockSeq {
    offset: usize,
    indent: usize,
    items: Vec<Item>,
}

struct FlowColl {
    offset: usize,
    indent: usize,
    start: Src,
    items: Vec<Item>,
    end: Vec<Src>,
}

struct Doc {
    offset: usize,
    start: Vec<Src>,
    value: Option<Box<Token>>,
    end: Option<Vec<Src>>,
}

struct DocEndTok {
    offset: usize,
    len: usize,
    end: Option<Vec<Src>>,
}

enum Token {
    /// byte-order-mark, space, comment, newline (stream level)
    Src(Src),
    Directive(Src),
    Document(Box<Doc>),
    DocEnd(DocEndTok),
    Error(ErrTok),
    Flow(FlowScalar),
    Block(Box<BlockScalar>),
    Map(Box<BlockMap>),
    Seq(Box<BlockSeq>),
    FlowColl(Box<FlowColl>),
}

impl Token {
    fn ty(&self) -> Ty {
        match self {
            Token::Src(s) => s.ty,
            Token::Directive(_) => Ty::Directive,
            Token::Document(_) => Ty::Document,
            Token::DocEnd(_) => Ty::DocEnd,
            Token::Error(_) => Ty::Error,
            Token::Flow(f) => f.ty,
            Token::Block(_) => Ty::BlockScalar,
            Token::Map(_) => Ty::BlockMap,
            Token::Seq(_) => Ty::BlockSeq,
            Token::FlowColl(_) => Ty::FlowCollection,
        }
    }

    fn offset(&self) -> usize {
        match self {
            Token::Src(s) | Token::Directive(s) => s.offset,
            Token::Document(d) => d.offset,
            Token::DocEnd(d) => d.offset,
            Token::Error(e) => e.offset,
            Token::Flow(f) => f.offset,
            Token::Block(b) => b.offset,
            Token::Map(m) => m.offset,
            Token::Seq(s) => s.offset,
            Token::FlowColl(f) => f.offset,
        }
    }

    /// `token.indent` if the token has one (`'indent' in token`)
    fn indent(&self) -> Option<usize> {
        match self {
            Token::Src(s) => Some(s.indent),
            Token::Flow(f) => Some(f.indent),
            Token::Block(b) => Some(b.indent),
            Token::Map(m) => Some(m.indent),
            Token::Seq(s) => Some(s.indent),
            Token::FlowColl(f) => Some(f.indent),
            Token::Directive(_) | Token::Document(_) | Token::DocEnd(_) | Token::Error(_) => None,
        }
    }

    /// Existing `token.end` array (`'end' in token`)
    fn end_existing(&mut self) -> Option<&mut Vec<Src>> {
        match self {
            Token::Flow(f) => f.end.as_mut(),
            Token::FlowColl(f) => Some(&mut f.end),
            Token::Document(d) => d.end.as_mut(),
            Token::DocEnd(d) => d.end.as_mut(),
            _ => None,
        }
    }

    /// `token.end` array, created when missing
    fn end_or_create(&mut self) -> Option<&mut Vec<Src>> {
        match self {
            Token::Flow(f) => Some(f.end.get_or_insert_with(Vec::new)),
            Token::FlowColl(f) => Some(&mut f.end),
            Token::Document(d) => Some(d.end.get_or_insert_with(Vec::new)),
            Token::DocEnd(d) => Some(d.end.get_or_insert_with(Vec::new)),
            _ => None,
        }
    }
}

fn is_flow_token(token: Option<&Token>) -> bool {
    matches!(token.map(Token::ty), Some(Ty::Alias | Ty::Scalar | Ty::SingleQuotedScalar | Ty::DoubleQuotedScalar | Ty::FlowCollection))
}

fn includes_token(list: &[Src], ty: Ty) -> bool {
    list.iter().any(|s| s.ty == ty)
}

fn find_non_empty_index(list: &[Src]) -> Option<usize> {
    list.iter().position(|s| !matches!(s.ty, Ty::Space | Ty::Comment | Ty::Newline))
}

fn prev_props_mut(parent: &mut Token) -> Option<&mut Vec<Src>> {
    match parent {
        Token::Document(d) => Some(&mut d.start),
        Token::Map(m) => {
            let it = m.items.last_mut()?;
            if it.sep.is_some() { it.sep.as_mut() } else { Some(&mut it.start) }
        }
        Token::Seq(s) => s.items.last_mut().map(|it| &mut it.start),
        _ => None,
    }
}

/// Note: May modify input array
fn get_first_key_start_props(prev: &mut Vec<Src>) -> Vec<Src> {
    if prev.is_empty() {
        return Vec::new();
    }
    let mut i = prev.len() as isize;
    loop {
        i -= 1;
        if i < 0 {
            break;
        }
        if matches!(prev[i as usize].ty, Ty::DocStart | Ty::ExplicitKeyInd | Ty::MapValueInd | Ty::SeqItemInd | Ty::Newline) {
            break;
        }
    }
    loop {
        i += 1;
        if prev.get(i as usize).map(|s| s.ty) != Some(Ty::Space) {
            break;
        }
    }
    prev.split_off((i as usize).min(prev.len()))
}

fn fix_flow_seq_items(fc: &mut FlowColl) {
    if fc.start.ty != Ty::FlowSeqStart {
        return;
    }
    for it in fc.items.iter_mut() {
        let Some(sep) = it.sep.as_ref() else { continue };
        if it.value.is_none() && !includes_token(&it.start, Ty::ExplicitKeyInd) && !includes_token(sep, Ty::MapValueInd) {
            if let Some(k) = it.key.take() {
                it.value = Some(k);
            }
            let sep = it.sep.take().unwrap_or_default();
            if is_flow_token(it.value.as_deref()) {
                if let Some(v) = it.value.as_deref_mut().and_then(Token::end_or_create) {
                    v.extend(sep);
                }
            } else {
                it.start.extend(sep);
            }
        }
    }
}

/// The empty trailing item cleanup in `Parser.pop()` for a block collection
/// that was just attached to a document, block map or block seq.
fn trim_trailing_items(token: &mut Token) -> Option<Vec<Src>> {
    let (items, indent) = match token {
        Token::Map(m) => (&mut m.items, m.indent),
        Token::Seq(s) => (&mut s.items, s.indent),
        _ => return None,
    };
    let last = items.last()?;
    if last.sep.is_none()
        && last.value.is_none()
        && !last.start.is_empty()
        && find_non_empty_index(&last.start).is_none()
        && (indent == 0 || last.start.iter().all(|st| st.ty != Ty::Comment || st.indent < indent))
    {
        return items.pop().map(|it| it.start);
    }
    None
}

fn at_indented_comment(ty: Ty, cur_indent: usize, start: &[Src], indent: usize) -> bool {
    if ty != Ty::Comment {
        return false;
    }
    if cur_indent <= indent {
        return false;
    }
    start.iter().all(|st| st.ty == Ty::Newline || st.ty == Ty::Space)
}

/// Drop a token tree without recursion (deeply nested input).
fn dispose_tokens(tokens: Vec<Token>) {
    let mut work = tokens;
    while let Some(t) = work.pop() {
        match t {
            Token::Document(mut d) => {
                if let Some(v) = d.value.take() {
                    work.push(*v);
                }
            }
            Token::Map(mut m) => {
                for it in m.items.drain(..) {
                    work.extend(it.key.into_iter().map(|b| *b));
                    work.extend(it.value.into_iter().map(|b| *b));
                }
            }
            Token::Seq(mut s) => {
                for it in s.items.drain(..) {
                    work.extend(it.key.into_iter().map(|b| *b));
                    work.extend(it.value.into_iter().map(|b| *b));
                }
            }
            Token::FlowColl(mut f) => {
                for it in f.items.drain(..) {
                    work.extend(it.key.into_iter().map(|b| *b));
                    work.extend(it.value.into_iter().map(|b| *b));
                }
            }
            Token::Block(mut b) => work.append(&mut b.props),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// CST parser (parse/parser.js)
// ---------------------------------------------------------------------------

struct Parser<'a> {
    src: &'a str,
    /// If true, space and sequence indicators count as indentation
    at_new_line: bool,
    /// If true, next token is a scalar value
    at_scalar: bool,
    /// Current indentation level
    indent: usize,
    /// Current offset since the start of parsing
    offset: usize,
    /// On the same line with a block map key
    on_key_line: bool,
    /// Top indicates the node that's currently being built
    stack: Vec<Token>,
    /// The type of the current token
    ty: Ty,
    /// Byte length of the current lexeme (1 for control lexemes)
    cur_len: usize,
    cur_ctl: Option<char>,
    /// Output tokens, and the number of known line starts when each was emitted
    out: Vec<Token>,
    out_lines: Vec<usize>,
    line_starts: Vec<usize>,
    /// deepest the token stack has been
    max_stack: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            src,
            at_new_line: true,
            at_scalar: false,
            indent: 0,
            offset: 0,
            on_key_line: false,
            stack: Vec::new(),
            ty: Ty::Newline,
            cur_len: 0,
            cur_ctl: None,
            out: Vec::new(),
            out_lines: Vec::new(),
            line_starts: Vec::new(),
            max_stack: 0,
        }
    }

    fn parse(&mut self, lexemes: &[Lex]) {
        self.line_starts.push(0);
        for lx in lexemes {
            self.next(*lx);
        }
        self.end();
    }

    fn emit(&mut self, t: Token) {
        self.out.push(t);
        self.out_lines.push(self.line_starts.len());
    }

    fn cur_source(&self) -> String {
        match self.cur_ctl {
            Some(c) => c.to_string(),
            None => self.src.get(self.offset..self.offset + self.cur_len).unwrap_or("").to_string(),
        }
    }

    fn error_token(&self, message: String) -> Token {
        Token::Error(ErrTok { offset: self.offset, message, source: self.cur_source(), len: self.cur_len })
    }

    fn source_token(&self) -> Src {
        Src { ty: self.ty, offset: self.offset, indent: self.indent, len: self.cur_len }
    }

    /// Advance the parser by the `source` of one lexical token.
    fn next(&mut self, lx: Lex) {
        let (ty, len, ctl) = match lx {
            Lex::Doc => (Some(Ty::DocMode), 1, Some('\x02')),
            Lex::FlowEnd => (Some(Ty::FlowErrorEnd), 1, Some('\x18')),
            Lex::Scalar => (Some(Ty::Scalar), 1, Some('\x1f')),
            Lex::Text(a, b) => {
                debug_assert_eq!(self.offset, a);
                (token_type(self.src.get(a..b).unwrap_or("")), b - a, None)
            }
        };
        self.cur_len = len;
        self.cur_ctl = ctl;
        if self.at_scalar {
            self.at_scalar = false;
            self.step();
            self.offset += len;
            return;
        }
        match ty {
            None => {
                let message = format!("Not a YAML token: {}", self.cur_source());
                let t = self.error_token(message);
                self.pop(Some(t));
                self.offset += len;
            }
            Some(Ty::Scalar) => {
                self.at_new_line = false;
                self.at_scalar = true;
                self.ty = Ty::Scalar;
            }
            Some(ty) => {
                self.ty = ty;
                self.step();
                match ty {
                    Ty::Newline => {
                        self.at_new_line = true;
                        self.indent = 0;
                        self.line_starts.push(self.offset + len);
                    }
                    Ty::Space => {
                        if self.at_new_line && self.src.as_bytes().get(self.offset) == Some(&b' ') {
                            self.indent += len;
                        }
                    }
                    Ty::ExplicitKeyInd | Ty::MapValueInd | Ty::SeqItemInd => {
                        if self.at_new_line {
                            self.indent += len;
                        }
                    }
                    Ty::DocMode | Ty::FlowErrorEnd => return,
                    _ => self.at_new_line = false,
                }
                self.offset += len;
            }
        }
    }

    /// Call at end of input to push out any remaining constructions
    fn end(&mut self) {
        while !self.stack.is_empty() {
            self.pop(None);
        }
    }

    fn step(&mut self) {
        while self.step_once() {}
        self.max_stack = self.max_stack.max(self.stack.len());
    }

    /// Returns true when the JS would call `this.step()` again (after a pop).
    fn step_once(&mut self) -> bool {
        let top_ty = self.stack.last().map(Token::ty);
        if self.ty == Ty::DocEnd && top_ty != Some(Ty::DocEnd) {
            while !self.stack.is_empty() {
                self.pop(None);
            }
            self.stack.push(Token::DocEnd(DocEndTok { offset: self.offset, len: self.cur_len, end: None }));
            return false;
        }
        match top_ty {
            None => {
                self.stream();
                false
            }
            Some(Ty::Document) => self.document(),
            Some(Ty::Alias | Ty::Scalar | Ty::SingleQuotedScalar | Ty::DoubleQuotedScalar) => self.scalar(),
            Some(Ty::BlockScalar) => self.block_scalar(),
            Some(Ty::BlockMap) => self.block_map(),
            Some(Ty::BlockSeq) => self.block_sequence(),
            Some(Ty::FlowCollection) => self.flow_collection(),
            Some(Ty::DocEnd) => {
                self.document_end();
                false
            }
            Some(_) => {
                self.pop(None);
                false
            }
        }
    }

    fn pop(&mut self, error: Option<Token>) {
        let token = match error {
            Some(t) => Some(t),
            None => self.stack.pop(),
        };
        let Some(mut token) = token else {
            let t = Token::Error(ErrTok {
                offset: self.offset,
                message: "Tried to pop an empty stack".to_string(),
                source: String::new(),
                len: 0,
            });
            self.emit(t);
            return;
        };
        if self.stack.is_empty() {
            self.emit(token);
            return;
        }
        let top_ty = self.stack.last().map(Token::ty).unwrap_or(Ty::Document);
        let top_indent = self.stack.last().and_then(Token::indent);
        match &mut token {
            Token::Block(bs) => bs.indent = top_indent.unwrap_or(0),
            Token::FlowColl(fc) if top_ty == Ty::Document => {
                // Ignore all indent for top-level flow collections
                fc.indent = 0;
            }
            _ => {}
        }
        if let Token::FlowColl(fc) = &mut token {
            fix_flow_seq_items(fc);
        }
        match top_ty {
            Ty::Document => {
                let extra = trim_trailing_items(&mut token);
                if let Some(Token::Document(d)) = self.stack.last_mut() {
                    d.value = Some(Box::new(token));
                    if let Some(s) = extra {
                        d.end = Some(s);
                    }
                }
            }
            Ty::BlockScalar => {
                if let Some(Token::Block(b)) = self.stack.last_mut() {
                    b.props.push(token); // error
                }
            }
            Ty::BlockMap => {
                let Some(Token::Map(m)) = self.stack.last_mut() else {
                    return;
                };
                let Some(it) = m.items.last_mut() else { return };
                if it.value.is_some() {
                    m.items.push(Item { key: Some(Box::new(token)), sep: Some(Vec::new()), ..Item::default() });
                    self.on_key_line = true;
                } else if it.sep.is_some() {
                    let extra = trim_trailing_items(&mut token);
                    it.value = Some(Box::new(token));
                    if let Some(s) = extra {
                        m.items.push(Item::with_start(s));
                    }
                } else {
                    it.key = Some(Box::new(token));
                    it.sep = Some(Vec::new());
                    self.on_key_line = !it.explicit_key;
                }
            }
            Ty::BlockSeq => {
                let extra = trim_trailing_items(&mut token);
                let Some(Token::Seq(s)) = self.stack.last_mut() else {
                    return;
                };
                match s.items.last_mut() {
                    Some(it) if it.value.is_none() => it.value = Some(Box::new(token)),
                    _ => s.items.push(Item { value: Some(Box::new(token)), ..Item::default() }),
                }
                if let Some(e) = extra {
                    s.items.push(Item::with_start(e));
                }
            }
            Ty::FlowCollection => {
                let Some(Token::FlowColl(fc)) = self.stack.last_mut() else {
                    return;
                };
                match fc.items.last_mut() {
                    Some(it) if it.value.is_none() && it.sep.is_some() => {
                        it.value = Some(Box::new(token));
                    }
                    Some(it) if it.value.is_none() => {
                        it.key = Some(Box::new(token));
                        it.sep = Some(Vec::new());
                    }
                    _ => fc.items.push(Item { key: Some(Box::new(token)), sep: Some(Vec::new()), ..Item::default() }),
                }
            }
            _ => {
                self.pop(None);
                self.pop(Some(token));
            }
        }
    }

    fn stream(&mut self) {
        match self.ty {
            Ty::DirectiveLine => {
                let mut s = self.source_token();
                s.ty = Ty::Directive;
                self.emit(Token::Directive(s));
            }
            Ty::ByteOrderMark | Ty::Space | Ty::Comment | Ty::Newline => {
                let s = self.source_token();
                self.emit(Token::Src(s));
            }
            Ty::DocMode | Ty::DocStart => {
                let mut doc = Doc { offset: self.offset, start: Vec::new(), value: None, end: None };
                if self.ty == Ty::DocStart {
                    doc.start.push(self.source_token());
                }
                self.stack.push(Token::Document(Box::new(doc)));
            }
            _ => {
                let t = self.error_token(format!("Unexpected {} token in YAML stream", self.ty.name()));
                self.emit(t);
            }
        }
    }

    fn top_doc(&mut self) -> &mut Doc {
        match self.stack.last_mut() {
            Some(Token::Document(d)) => d,
            _ => unreachable!("document expected on top of the stack"),
        }
    }

    fn top_block_scalar(&mut self) -> &mut BlockScalar {
        match self.stack.last_mut() {
            Some(Token::Block(b)) => b,
            _ => unreachable!("block scalar expected on top of the stack"),
        }
    }

    fn top_map(&mut self) -> &mut BlockMap {
        match self.stack.last_mut() {
            Some(Token::Map(m)) => m,
            _ => unreachable!("block map expected on top of the stack"),
        }
    }

    fn top_seq(&mut self) -> &mut BlockSeq {
        match self.stack.last_mut() {
            Some(Token::Seq(s)) => s,
            _ => unreachable!("block seq expected on top of the stack"),
        }
    }

    fn top_fc(&mut self) -> &mut FlowColl {
        match self.stack.last_mut() {
            Some(Token::FlowColl(f)) => f,
            _ => unreachable!("flow collection expected on top of the stack"),
        }
    }

    fn document(&mut self) -> bool {
        let src = self.source_token();
        if self.top_doc().value.is_some() {
            return self.line_end();
        }
        match self.ty {
            Ty::DocStart => {
                if find_non_empty_index(&self.top_doc().start).is_some() {
                    self.pop(None);
                    return true;
                }
                self.top_doc().start.push(src);
                return false;
            }
            Ty::Anchor | Ty::Tag | Ty::Space | Ty::Comment | Ty::Newline => {
                self.top_doc().start.push(src);
                return false;
            }
            _ => {}
        }
        match self.start_block_value() {
            Some(bv) => self.stack.push(bv),
            None => {
                let t = self.error_token(format!("Unexpected {} token in YAML document", self.ty.name()));
                self.emit(t);
            }
        }
        false
    }

    fn scalar(&mut self) -> bool {
        if self.ty != Ty::MapValueInd {
            return self.line_end();
        }
        let src = self.source_token();
        let Some(Token::Flow(mut sc)) = self.stack.pop() else {
            return false;
        };
        let start = self.take_first_key_start_props();
        let sep = match sc.end.take() {
            Some(mut e) => {
                e.push(src);
                e
            }
            None => vec![src],
        };
        let (offset, indent) = (sc.offset, sc.indent);
        let map = BlockMap {
            offset,
            indent,
            items: vec![Item { start, key: Some(Box::new(Token::Flow(sc))), sep: Some(sep), ..Item::default() }],
        };
        self.on_key_line = true;
        self.stack.push(Token::Map(Box::new(map)));
        false
    }

    fn block_scalar(&mut self) -> bool {
        match self.ty {
            Ty::Space | Ty::Comment | Ty::Newline => {
                let s = self.source_token();
                self.top_block_scalar().props.push(Token::Src(s));
                false
            }
            Ty::Scalar => {
                let (off, len) = (self.offset, self.cur_len);
                {
                    let bs = self.top_block_scalar();
                    bs.src_off = off;
                    bs.src_len = len;
                }
                // block-scalar source includes trailing newline
                self.at_new_line = true;
                self.indent = 0;
                let bytes = self.src.as_bytes();
                for (i, b) in bytes[off..off + len].iter().enumerate() {
                    if *b == b'\n' {
                        self.line_starts.push(off + i + 1);
                    }
                }
                self.pop(None);
                false
            }
            _ => {
                self.pop(None);
                true
            }
        }
    }

    fn block_map(&mut self) -> bool {
        let src = self.source_token();
        let ty = self.ty;
        let cur_indent = self.indent;
        match ty {
            Ty::Newline => {
                self.on_key_line = false;
                let items = &mut self.top_map().items;
                let Some(last) = items.len().checked_sub(1) else {
                    return false;
                };
                if items[last].value.is_some() {
                    let appended = match items[last].value.as_deref_mut().and_then(Token::end_existing) {
                        Some(e) if matches!(e.last(), Some(s) if s.ty == Ty::Comment) => {
                            e.push(src);
                            true
                        }
                        _ => false,
                    };
                    if !appended {
                        items.push(Item::with_start(vec![src]));
                    }
                } else if let Some(sep) = items[last].sep.as_mut() {
                    sep.push(src);
                } else {
                    items[last].start.push(src);
                }
                return false;
            }
            Ty::Space | Ty::Comment => {
                let map_indent = self.top_map().indent;
                let items = &mut self.top_map().items;
                let n = items.len();
                if n == 0 {
                    return false;
                }
                if items[n - 1].value.is_some() {
                    items.push(Item::with_start(vec![src]));
                } else if let Some(sep) = items[n - 1].sep.as_mut() {
                    sep.push(src);
                } else {
                    let mut merged = false;
                    if n >= 2 && at_indented_comment(ty, cur_indent, &items[n - 1].start, map_indent) {
                        let (head, tail) = items.split_at_mut(n - 1);
                        if let Some(end) = head[n - 2].value.as_deref_mut().and_then(Token::end_existing) {
                            end.extend_from_slice(&tail[0].start);
                            end.push(src);
                            merged = true;
                        }
                    }
                    if merged {
                        items.pop();
                    } else {
                        items[n - 1].start.push(src);
                    }
                }
                return false;
            }
            _ => {}
        }
        let map_indent = self.top_map().indent;
        if cur_indent >= map_indent {
            let at_map_indent = !self.on_key_line && cur_indent == map_indent;
            let (it_sep, it_explicit, it_value) = {
                let m = self.top_map();
                match m.items.last() {
                    Some(it) => (it.sep.is_some(), it.explicit_key, it.value.is_some()),
                    None => (false, false, false),
                }
            };
            let at_next_item = at_map_indent && (it_sep || it_explicit) && ty != Ty::SeqItemInd;
            // For empty nodes, assign newline-separated not indented empty tokens to following node
            let mut start: Vec<Src> = Vec::new();
            if at_next_item && it_sep && !it_value {
                let m = self.top_map();
                if let Some(sep) = m.items.last_mut().and_then(|it| it.sep.as_mut()) {
                    let mut nl: Vec<usize> = Vec::new();
                    for (i, st) in sep.iter().enumerate() {
                        match st.ty {
                            Ty::Newline => nl.push(i),
                            Ty::Space => {}
                            Ty::Comment => {
                                if st.indent > map_indent {
                                    nl.clear();
                                }
                            }
                            _ => nl.clear(),
                        }
                    }
                    if nl.len() >= 2 {
                        start = sep.split_off(nl[1]);
                    }
                }
            }
            match ty {
                Ty::Anchor | Ty::Tag => {
                    let m = self.top_map();
                    if at_next_item || it_value {
                        start.push(src);
                        m.items.push(Item::with_start(start));
                        self.on_key_line = true;
                    } else if let Some(sep) = m.items.last_mut().and_then(|it| it.sep.as_mut()) {
                        sep.push(src);
                    } else if let Some(it) = m.items.last_mut() {
                        it.start.push(src);
                    }
                    return false;
                }
                Ty::ExplicitKeyInd => {
                    let (offset, indent) = (self.offset, self.indent);
                    let m = self.top_map();
                    let (has_sep, explicit) = match m.items.last() {
                        Some(it) => (it.sep.is_some(), it.explicit_key),
                        None => (false, false),
                    };
                    if !has_sep && !explicit {
                        if let Some(it) = m.items.last_mut() {
                            it.start.push(src);
                            it.explicit_key = true;
                        }
                    } else if at_next_item || it_value {
                        start.push(src);
                        m.items.push(Item { start, explicit_key: true, ..Item::default() });
                    } else {
                        self.stack.push(Token::Map(Box::new(BlockMap {
                            offset,
                            indent,
                            items: vec![Item { start: vec![src], explicit_key: true, ..Item::default() }],
                        })));
                    }
                    self.on_key_line = true;
                    return false;
                }
                Ty::MapValueInd => {
                    self.block_map_value_ind(src, start, at_next_item);
                    self.on_key_line = true;
                    return false;
                }
                Ty::Alias | Ty::Scalar | Ty::SingleQuotedScalar | Ty::DoubleQuotedScalar => {
                    let fs = self.flow_scalar(ty);
                    let m = self.top_map();
                    let has_sep = m.items.last().is_some_and(|it| it.sep.is_some());
                    if at_next_item || it_value {
                        m.items.push(Item { start, key: Some(Box::new(fs)), sep: Some(Vec::new()), ..Item::default() });
                        self.on_key_line = true;
                    } else if has_sep {
                        self.stack.push(fs);
                    } else {
                        if let Some(it) = m.items.last_mut() {
                            it.key = Some(Box::new(fs));
                            it.sep = Some(Vec::new());
                        }
                        self.on_key_line = true;
                    }
                    return false;
                }
                _ => {
                    if let Some(bv) = self.start_block_value() {
                        if matches!(bv, Token::Seq(_)) {
                            let bad = {
                                let m = self.top_map();
                                m.items.last().is_some_and(|it| {
                                    !it.explicit_key && it.sep.as_ref().is_some_and(|sep| !includes_token(sep, Ty::Newline))
                                })
                            };
                            if bad {
                                let t = self.error_token("Unexpected block-seq-ind on same line with key".to_string());
                                self.pop(Some(t));
                                return false;
                            }
                        } else if at_map_indent {
                            self.top_map().items.push(Item::with_start(start));
                        }
                        self.stack.push(bv);
                        return false;
                    }
                }
            }
        }
        self.pop(None);
        true
    }

    /// The `case 'map-value-ind'` of `blockMap()`; `start` is the (maybe empty) outer `start`.
    fn block_map_value_ind(&mut self, src: Src, start: Vec<Src>, at_next_item: bool) {
        let (offset, indent) = (self.offset, self.indent);
        let m = self.top_map();
        let Some(last) = m.items.len().checked_sub(1) else {
            return;
        };
        let new_map = |items: Vec<Item>| Token::Map(Box::new(BlockMap { offset, indent, items }));
        let mut push_map: Option<Token> = None;
        let mut push_item: Option<Item> = None;
        {
            let it = &mut m.items[last];
            if it.explicit_key {
                if it.sep.is_none() {
                    if includes_token(&it.start, Ty::Newline) {
                        it.key = None;
                        it.sep = Some(vec![src]);
                    } else {
                        let start2 = get_first_key_start_props(&mut it.start);
                        push_map = Some(new_map(vec![Item { start: start2, key: None, sep: Some(vec![src]), ..Item::default() }]));
                    }
                } else if it.value.is_some() {
                    push_item = Some(Item { key: None, sep: Some(vec![src]), ..Item::default() });
                } else if it.sep.as_ref().is_some_and(|s| includes_token(s, Ty::MapValueInd)) {
                    push_map = Some(new_map(vec![Item { start, key: None, sep: Some(vec![src]), ..Item::default() }]));
                } else if is_flow_token(it.key.as_deref()) && !it.sep.as_ref().is_some_and(|s| includes_token(s, Ty::Newline)) {
                    let start2 = get_first_key_start_props(&mut it.start);
                    let key = it.key.take();
                    let mut sep = it.sep.take().unwrap_or_default();
                    sep.push(src);
                    push_map = Some(new_map(vec![Item { start: start2, key, sep: Some(sep), ..Item::default() }]));
                } else if !start.is_empty() {
                    // Not actually at next item
                    if let Some(sep) = it.sep.as_mut() {
                        sep.extend(start);
                        sep.push(src);
                    }
                } else if let Some(sep) = it.sep.as_mut() {
                    sep.push(src);
                }
            } else if it.sep.is_none() {
                it.key = None;
                it.sep = Some(vec![src]);
            } else if it.value.is_some() || at_next_item {
                push_item = Some(Item { start, key: None, sep: Some(vec![src]), ..Item::default() });
            } else if it.sep.as_ref().is_some_and(|s| includes_token(s, Ty::MapValueInd)) {
                push_map = Some(new_map(vec![Item { key: None, sep: Some(vec![src]), ..Item::default() }]));
            } else if let Some(sep) = it.sep.as_mut() {
                sep.push(src);
            }
        }
        if let Some(it) = push_item {
            m.items.push(it);
        }
        if let Some(t) = push_map {
            self.stack.push(t);
        }
    }

    fn block_sequence(&mut self) -> bool {
        let src = self.source_token();
        let ty = self.ty;
        let cur_indent = self.indent;
        let seq_indent = self.top_seq().indent;
        match ty {
            Ty::Newline => {
                let items = &mut self.top_seq().items;
                let Some(last) = items.len().checked_sub(1) else {
                    return false;
                };
                if items[last].value.is_some() {
                    let appended = match items[last].value.as_deref_mut().and_then(Token::end_existing) {
                        Some(e) if matches!(e.last(), Some(s) if s.ty == Ty::Comment) => {
                            e.push(src);
                            true
                        }
                        _ => false,
                    };
                    if !appended {
                        items.push(Item::with_start(vec![src]));
                    }
                } else {
                    items[last].start.push(src);
                }
                return false;
            }
            Ty::Space | Ty::Comment => {
                let items = &mut self.top_seq().items;
                let n = items.len();
                if n == 0 {
                    return false;
                }
                if items[n - 1].value.is_some() {
                    items.push(Item::with_start(vec![src]));
                } else {
                    let mut merged = false;
                    if n >= 2 && at_indented_comment(ty, cur_indent, &items[n - 1].start, seq_indent) {
                        let (head, tail) = items.split_at_mut(n - 1);
                        if let Some(end) = head[n - 2].value.as_deref_mut().and_then(Token::end_existing) {
                            end.extend_from_slice(&tail[0].start);
                            end.push(src);
                            merged = true;
                        }
                    }
                    if merged {
                        items.pop();
                    } else {
                        items[n - 1].start.push(src);
                    }
                }
                return false;
            }
            Ty::Anchor | Ty::Tag => {
                let items = &mut self.top_seq().items;
                let has_value = items.last().is_some_and(|it| it.value.is_some());
                if !(has_value || cur_indent <= seq_indent) {
                    if let Some(it) = items.last_mut() {
                        it.start.push(src);
                    }
                    return false;
                }
            }
            Ty::SeqItemInd if cur_indent == seq_indent => {
                let items = &mut self.top_seq().items;
                let push_new = items.last().is_some_and(|it| it.value.is_some() || includes_token(&it.start, Ty::SeqItemInd));
                if push_new {
                    items.push(Item::with_start(vec![src]));
                } else if let Some(it) = items.last_mut() {
                    it.start.push(src);
                }
                return false;
            }
            _ => {}
        }
        if cur_indent > seq_indent
            && let Some(bv) = self.start_block_value()
        {
            self.stack.push(bv);
            return false;
        }
        self.pop(None);
        true
    }

    fn flow_collection(&mut self) -> bool {
        let src = self.source_token();
        let ty = self.ty;
        if ty == Ty::FlowErrorEnd {
            loop {
                self.pop(None);
                if self.stack.last().map(Token::ty) != Some(Ty::FlowCollection) {
                    break;
                }
            }
            return false;
        }
        if self.top_fc().end.is_empty() {
            match ty {
                Ty::Comma | Ty::ExplicitKeyInd => {
                    let fc = self.top_fc();
                    match fc.items.last_mut() {
                        Some(it) if it.sep.is_none() => it.start.push(src),
                        _ => fc.items.push(Item::with_start(vec![src])),
                    }
                    return false;
                }
                Ty::MapValueInd => {
                    let fc = self.top_fc();
                    match fc.items.last_mut() {
                        Some(it) if it.value.is_none() => {
                            if let Some(sep) = it.sep.as_mut() {
                                sep.push(src);
                            } else {
                                it.key = None;
                                it.sep = Some(vec![src]);
                            }
                        }
                        _ => fc.items.push(Item { key: None, sep: Some(vec![src]), ..Item::default() }),
                    }
                    return false;
                }
                Ty::Space | Ty::Comment | Ty::Newline | Ty::Anchor | Ty::Tag => {
                    let fc = self.top_fc();
                    match fc.items.last_mut() {
                        Some(it) if it.value.is_none() => {
                            if let Some(sep) = it.sep.as_mut() {
                                sep.push(src);
                            } else {
                                it.start.push(src);
                            }
                        }
                        _ => fc.items.push(Item::with_start(vec![src])),
                    }
                    return false;
                }
                Ty::Alias | Ty::Scalar | Ty::SingleQuotedScalar | Ty::DoubleQuotedScalar => {
                    let fs = self.flow_scalar(ty);
                    let fc = self.top_fc();
                    match fc.items.last_mut() {
                        Some(it) if it.value.is_none() => {
                            if it.sep.is_some() {
                                self.stack.push(fs);
                            } else {
                                it.key = Some(Box::new(fs));
                                it.sep = Some(Vec::new());
                            }
                        }
                        _ => fc.items.push(Item { key: Some(Box::new(fs)), sep: Some(Vec::new()), ..Item::default() }),
                    }
                    return false;
                }
                Ty::FlowMapEnd | Ty::FlowSeqEnd => {
                    self.top_fc().end.push(src);
                    return false;
                }
                _ => {}
            }
            match self.start_block_value() {
                Some(bv) => {
                    self.stack.push(bv);
                    false
                }
                None => {
                    self.pop(None);
                    true
                }
            }
        } else {
            let n = self.stack.len();
            let fc_indent = self.top_fc().indent;
            let (parent_ty, parent_indent, parent_last_has_sep) = if n >= 2 {
                match &self.stack[n - 2] {
                    Token::Map(m) => (Ty::BlockMap, m.indent, m.items.last().is_some_and(|it| it.sep.is_some())),
                    other => (other.ty(), other.indent().unwrap_or(0), false),
                }
            } else {
                (Ty::Document, 0, false)
            };
            if parent_ty == Ty::BlockMap
                && ((ty == Ty::MapValueInd && parent_indent == fc_indent) || (ty == Ty::Newline && !parent_last_has_sep))
            {
                self.pop(None);
                true
            } else if ty == Ty::MapValueInd && parent_ty != Ty::FlowCollection {
                // getPrevProps(parent) must be taken while the flow collection is
                // still on the stack top
                let Some(Token::FlowColl(mut fc)) = self.stack.pop() else {
                    return false;
                };
                let start = self.take_first_key_start_props();
                fix_flow_seq_items(&mut fc);
                let mut sep: Vec<Src> = if fc.end.len() > 1 { fc.end.split_off(1) } else { Vec::new() };
                sep.push(src);
                let (offset, indent) = (fc.offset, fc.indent);
                let map = BlockMap {
                    offset,
                    indent,
                    items: vec![Item { start, key: Some(Box::new(Token::FlowColl(fc))), sep: Some(sep), ..Item::default() }],
                };
                self.on_key_line = true;
                self.stack.push(Token::Map(Box::new(map)));
                false
            } else {
                self.line_end()
            }
        }
    }

    fn flow_scalar(&mut self, ty: Ty) -> Token {
        let bytes = self.src.as_bytes();
        let (off, len) = (self.offset, self.cur_len);
        if let Some(s) = bytes.get(off..off + len) {
            for (i, b) in s.iter().enumerate() {
                if *b == b'\n' {
                    self.line_starts.push(off + i + 1);
                }
            }
        }
        Token::Flow(FlowScalar { ty, offset: off, indent: self.indent, len, end: None })
    }

    fn take_first_key_start_props(&mut self) -> Vec<Src> {
        match self.stack.last_mut().and_then(prev_props_mut) {
            Some(prev) => get_first_key_start_props(prev),
            None => Vec::new(),
        }
    }

    /// `startBlockValue(parent)` where `parent` is always the stack top.
    fn start_block_value(&mut self) -> Option<Token> {
        let src = self.source_token();
        let (offset, indent) = (self.offset, self.indent);
        match self.ty {
            Ty::Alias | Ty::Scalar | Ty::SingleQuotedScalar | Ty::DoubleQuotedScalar => Some(self.flow_scalar(self.ty)),
            Ty::BlockScalarHeader => {
                Some(Token::Block(Box::new(BlockScalar { offset, indent, props: vec![Token::Src(src)], src_off: 0, src_len: 0 })))
            }
            Ty::FlowMapStart | Ty::FlowSeqStart => {
                Some(Token::FlowColl(Box::new(FlowColl { offset, indent, start: src, items: Vec::new(), end: Vec::new() })))
            }
            Ty::SeqItemInd => Some(Token::Seq(Box::new(BlockSeq { offset, indent, items: vec![Item::with_start(vec![src])] }))),
            Ty::ExplicitKeyInd => {
                self.on_key_line = true;
                let mut start = self.take_first_key_start_props();
                start.push(src);
                Some(Token::Map(Box::new(BlockMap { offset, indent, items: vec![Item { start, explicit_key: true, ..Item::default() }] })))
            }
            Ty::MapValueInd => {
                self.on_key_line = true;
                let start = self.take_first_key_start_props();
                Some(Token::Map(Box::new(BlockMap {
                    offset,
                    indent,
                    items: vec![Item { start, key: None, sep: Some(vec![src]), ..Item::default() }],
                })))
            }
            _ => None,
        }
    }

    fn document_end(&mut self) {
        if self.ty != Ty::DocMode {
            let src = self.source_token();
            if let Some(Token::DocEnd(d)) = self.stack.last_mut() {
                d.end.get_or_insert_with(Vec::new).push(src);
            }
            if self.ty == Ty::Newline {
                self.pop(None);
            }
        }
    }

    fn line_end(&mut self) -> bool {
        match self.ty {
            Ty::Comma | Ty::DocStart | Ty::DocEnd | Ty::FlowSeqEnd | Ty::FlowMapEnd | Ty::MapValueInd => {
                self.pop(None);
                return true;
            }
            Ty::Newline => self.on_key_line = false,
            _ => {}
        }
        // all other values are errors
        let src = self.source_token();
        if let Some(end) = self.stack.last_mut().and_then(Token::end_or_create) {
            end.push(src);
        }
        if self.ty == Ty::Newline {
            self.pop(None);
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Directives (doc/directives.js)
// ---------------------------------------------------------------------------

const DEFAULT_TAG_PREFIX: &str = "tag:yaml.org,2002:";
const MAP_TAG: &str = "tag:yaml.org,2002:map";
const SEQ_TAG: &str = "tag:yaml.org,2002:seq";
const STR_TAG: &str = "tag:yaml.org,2002:str";

#[derive(Clone)]
struct Directives {
    yaml_explicit: bool,
    yaml_version: &'static str,
    tags: Vec<(String, String)>,
    at_next_document: bool,
}

fn default_tags() -> Vec<(String, String)> {
    vec![("!!".to_string(), DEFAULT_TAG_PREFIX.to_string())]
}

impl Directives {
    fn new() -> Self {
        Directives { yaml_explicit: false, yaml_version: "1.2", tags: default_tags(), at_next_document: false }
    }

    /// During parsing, get a Directives instance for the current document and
    /// update the stream state according to the current version's spec.
    fn at_document(&mut self) -> Directives {
        let res = Directives {
            yaml_explicit: self.yaml_explicit,
            yaml_version: self.yaml_version,
            tags: self.tags.clone(),
            at_next_document: false,
        };
        match self.yaml_version {
            "1.1" => self.at_next_document = true,
            "1.2" => {
                self.at_next_document = false;
                self.yaml_explicit = false;
                self.yaml_version = "1.2";
                self.tags = default_tags();
            }
            _ => {}
        }
        res
    }

    fn set_tag(&mut self, handle: &str, prefix: &str) {
        if let Some(e) = self.tags.iter_mut().find(|(h, _)| h == handle) {
            e.1 = prefix.to_string();
        } else {
            self.tags.push((handle.to_string(), prefix.to_string()));
        }
    }

    /// `onError(offset, message, warning)` may be called even if the action was successful.
    fn add(&mut self, line: &str, on_error: &mut dyn FnMut(usize, String, bool)) -> bool {
        if self.at_next_document {
            self.yaml_explicit = false;
            self.yaml_version = "1.1";
            self.tags = default_tags();
            self.at_next_document = false;
        }
        let mut parts = js_trim(line).split([' ', '\t']).filter(|p| !p.is_empty());
        let name = parts.next().unwrap_or("");
        let parts: Vec<&str> = parts.collect();
        match name {
            "%TAG" => {
                if parts.len() != 2 {
                    on_error(0, "%TAG directive should contain exactly two parts".to_string(), false);
                    if parts.len() < 2 {
                        return false;
                    }
                }
                self.set_tag(parts[0], parts[1]);
                true
            }
            "%YAML" => {
                self.yaml_explicit = true;
                if parts.len() != 1 {
                    on_error(0, "%YAML directive should contain exactly one part".to_string(), false);
                    return false;
                }
                let version = parts[0];
                if version == "1.1" || version == "1.2" {
                    self.yaml_version = if version == "1.1" { "1.1" } else { "1.2" };
                    true
                } else {
                    let valid = version.split_once('.').is_some_and(|(a, b)| {
                        !a.is_empty() && !b.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit())
                    });
                    on_error(6, format!("Unsupported YAML version {version}"), valid);
                    false
                }
            }
            _ => {
                on_error(0, format!("Unknown directive {name}"), true);
                false
            }
        }
    }

    /// Resolves a tag, matching handles to those defined in %TAG directives.
    fn tag_name(&self, source: &str, on_error: &mut dyn FnMut(String)) -> Option<String> {
        if source == "!" {
            return Some("!".to_string()); // non-specific tag
        }
        if !source.starts_with('!') {
            on_error(format!("Not a valid tag: {source}"));
            return None;
        }
        if source.as_bytes().get(1) == Some(&b'<') {
            // source.slice(2, -1)
            let verbatim = if source.len() >= 3 {
                let end = source.len() - source.chars().next_back().map_or(0, char::len_utf8);
                source.get(2..end).unwrap_or("")
            } else {
                ""
            };
            if verbatim == "!" || verbatim == "!!" {
                on_error(format!("Verbatim tags aren't resolved, so {source} is invalid."));
                return None;
            }
            if !source.ends_with('>') {
                on_error("Verbatim tags must end with a >".to_string());
            }
            return Some(verbatim.to_string());
        }
        let bang = source.rfind('!').unwrap_or(0);
        let (handle, suffix) = (&source[..=bang], &source[bang + 1..]);
        if suffix.is_empty() {
            on_error(format!("The {source} tag has no suffix"));
        }
        let prefix = self.tags.iter().find(|(h, _)| h == handle).map(|(_, p)| p.as_str()).filter(|p| !p.is_empty());
        if let Some(prefix) = prefix {
            return match js_decode_uri_component(suffix) {
                Ok(s) => Some(format!("{prefix}{s}")),
                Err(()) => {
                    on_error("URIError: URI malformed".to_string());
                    None
                }
            };
        }
        if handle == "!" {
            return Some(source.to_string()); // local tag
        }
        on_error(format!("Could not resolve tag: {source}"));
        None
    }
}

// ---------------------------------------------------------------------------
// Core schema tags (schema/core, schema/common)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CoreTag {
    Str,
    Null,
    Bool,
    IntOct,
    Int,
    IntHex,
    FloatNaN,
    FloatExp,
    Float,
}

/// The scalar tags of the core schema in `schema.tags` order (map and seq are
/// collection tags and never apply to scalars).
const CORE_SCALAR_TAGS: [CoreTag; 9] = [
    CoreTag::Str,
    CoreTag::Null,
    CoreTag::Bool,
    CoreTag::IntOct,
    CoreTag::Int,
    CoreTag::IntHex,
    CoreTag::FloatNaN,
    CoreTag::FloatExp,
    CoreTag::Float,
];

impl CoreTag {
    fn uri(self) -> &'static str {
        match self {
            CoreTag::Str => STR_TAG,
            CoreTag::Null => "tag:yaml.org,2002:null",
            CoreTag::Bool => "tag:yaml.org,2002:bool",
            CoreTag::IntOct | CoreTag::Int | CoreTag::IntHex => "tag:yaml.org,2002:int",
            CoreTag::FloatNaN | CoreTag::FloatExp | CoreTag::Float => "tag:yaml.org,2002:float",
        }
    }

    /// `tag.test.test(value)` (false for the string tag, which has no test)
    fn test(self, v: &str) -> bool {
        let b = v.as_bytes();
        match self {
            CoreTag::Str => false,
            CoreTag::Null => matches!(v, "" | "~" | "null" | "Null" | "NULL"),
            CoreTag::Bool => matches!(v, "true" | "True" | "TRUE" | "false" | "False" | "FALSE"),
            CoreTag::IntOct => b.len() > 2 && b.starts_with(b"0o") && b[2..].iter().all(|c| (b'0'..=b'7').contains(c)),
            CoreTag::Int => {
                let d = if matches!(b.first(), Some(b'-' | b'+')) { &b[1..] } else { b };
                !d.is_empty() && d.iter().all(u8::is_ascii_digit)
            }
            CoreTag::IntHex => b.len() > 2 && b.starts_with(b"0x") && b[2..].iter().all(u8::is_ascii_hexdigit),
            CoreTag::FloatNaN => {
                let d = if matches!(b.first(), Some(b'-' | b'+')) { &b[1..] } else { b };
                matches!(d, b".inf" | b".Inf" | b".INF") || matches!(v, ".nan" | ".NaN" | ".NAN")
            }
            CoreTag::FloatExp => is_float(b, true),
            CoreTag::Float => is_float(b, false),
        }
    }

    fn resolve(self, v: &str) -> Value {
        match self {
            CoreTag::Str => Value::String(v.to_string()),
            CoreTag::Null => Value::Null,
            CoreTag::Bool => Value::Bool(matches!(v.as_bytes()[0], b't' | b'T')),
            CoreTag::IntOct => Value::Number(parse_radix(&v[2..], 8).unwrap_or(f64::NAN)),
            CoreTag::IntHex => Value::Number(parse_radix(&v[2..], 16).unwrap_or(f64::NAN)),
            CoreTag::Int => {
                let (neg, digits) = match v.as_bytes().first() {
                    Some(b'-') => (true, &v[1..]),
                    Some(b'+') => (false, &v[1..]),
                    _ => (false, v),
                };
                let n = parse_radix(digits, 10).unwrap_or(f64::NAN);
                Value::Number(if neg { -n } else { n })
            }
            CoreTag::FloatNaN => {
                if v[v.len() - 3..].eq_ignore_ascii_case("nan") {
                    Value::Number(f64::NAN)
                } else if v.starts_with('-') {
                    Value::Number(f64::NEG_INFINITY)
                } else {
                    Value::Number(f64::INFINITY)
                }
            }
            CoreTag::FloatExp | CoreTag::Float => Value::Number(v.parse::<f64>().unwrap_or(f64::NAN)),
        }
    }
}

/// `[-+]?(?:\.[0-9]+|[0-9]+(?:\.[0-9]*)?)[eE][-+]?[0-9]+` when `exp`, else
/// `[-+]?(?:\.[0-9]+|[0-9]+\.[0-9]*)`.
fn is_float(b: &[u8], exp: bool) -> bool {
    let digits = |i: usize| b[i.min(b.len())..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i = 1;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let n = digits(i);
        if n == 0 {
            return false;
        }
        i += n;
    } else {
        let n = digits(i);
        if n == 0 {
            return false;
        }
        i += n;
        if b.get(i) == Some(&b'.') {
            i += 1;
            i += digits(i);
        } else if !exp {
            return false;
        }
    }
    if !exp {
        return i == b.len();
    }
    if !matches!(b.get(i), Some(b'e' | b'E')) {
        return false;
    }
    i += 1;
    if matches!(b.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let n = digits(i);
    n > 0 && i + n == b.len()
}

/// Known tags of the (not ported) YAML 1.1 types: `Some(collection)` where
/// `collection` is `None` for scalar types.
fn known_tag(tag: &str) -> Option<Option<&'static str>> {
    match tag {
        "tag:yaml.org,2002:binary" | "tag:yaml.org,2002:merge" | "tag:yaml.org,2002:timestamp" => Some(None),
        "tag:yaml.org,2002:omap" | "tag:yaml.org,2002:pairs" => Some(Some("seq")),
        "tag:yaml.org,2002:set" => Some(Some("map")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Composer (compose/*.js)
// ---------------------------------------------------------------------------

struct Props {
    comma: Option<Src>,
    found: Option<Src>,
    comment: String,
    has_newline: bool,
    anchor: Option<Src>,
    tag: Option<Src>,
    newline_after_prop: Option<Src>,
    end: usize,
    start: usize,
}

#[derive(Clone, Copy)]
struct NextInfo {
    ty: Ty,
    offset: usize,
    /// `next.source === ''` for scalars
    empty_source: bool,
}

fn next_of_tok(t: &Token) -> NextInfo {
    NextInfo { ty: t.ty(), offset: t.offset(), empty_source: matches!(t, Token::Flow(f) if f.len == 0) }
}

fn next_of_src(s: &Src) -> NextInfo {
    NextInfo { ty: s.ty, offset: s.offset, empty_source: s.len == 0 }
}

struct Resolved {
    value: String,
    ty: Option<ScalarStyle>,
    comment: String,
    range: [usize; 3],
}

struct DocB {
    doc: Document,
    doc_start: bool,
}

fn pos_src(s: &Src) -> (usize, usize) {
    (s.offset, s.offset + s.len)
}

/// `getErrorPos(token)`
fn pos_tok(t: &Token) -> (usize, usize) {
    match t {
        Token::Src(s) | Token::Directive(s) => pos_src(s),
        Token::DocEnd(d) => (d.offset, d.offset + d.len),
        Token::Error(e) => (e.offset, e.offset + e.len),
        Token::Flow(f) => (f.offset, f.offset + f.len),
        Token::Block(b) => (b.offset, b.offset + b.src_len),
        Token::Document(d) => (d.offset, d.offset + 1),
        Token::Map(m) => (m.offset, m.offset + 1),
        Token::Seq(s) => (s.offset, s.offset + 1),
        Token::FlowColl(f) => (f.offset, f.offset + 1),
    }
}

fn new_node(kind: NodeKind, range: [usize; 3]) -> Node {
    Node { kind, range, anchor: None, tag: None, comment: None, comment_before: None }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

/// `util-contains-newline.js` (iterative: keys can be nested arbitrarily deep)
fn contains_newline(src: &str, key: Option<&Token>) -> bool {
    let mut work: Vec<&Token> = Vec::new();
    work.extend(key);
    while let Some(key) = work.pop() {
        match key {
            Token::Flow(f) => {
                if !matches!(f.ty, Ty::Alias | Ty::Scalar | Ty::DoubleQuotedScalar | Ty::SingleQuotedScalar) {
                    return true;
                }
                if src.as_bytes().get(f.offset..f.offset + f.len).is_some_and(|s| s.contains(&b'\n')) {
                    return true;
                }
                if let Some(end) = &f.end
                    && end.iter().any(|st| st.ty == Ty::Newline)
                {
                    return true;
                }
            }
            Token::FlowColl(fc) => {
                for it in &fc.items {
                    if it.start.iter().any(|st| st.ty == Ty::Newline) {
                        return true;
                    }
                    if let Some(sep) = &it.sep
                        && sep.iter().any(|st| st.ty == Ty::Newline)
                    {
                        return true;
                    }
                    work.extend(it.key.as_deref());
                    work.extend(it.value.as_deref());
                }
            }
            _ => return true,
        }
    }
    false
}

/// `util-empty-scalar-position.js`
fn empty_scalar_position(mut offset: usize, before: Option<&[Src]>) -> usize {
    if let Some(before) = before {
        let mut i = before.len() as isize - 1;
        while i >= 0 {
            let st = &before[i as usize];
            if matches!(st.ty, Ty::Space | Ty::Comment | Ty::Newline) {
                offset = offset.wrapping_sub(st.len);
                i -= 1;
                continue;
            }
            // Technically, an empty scalar is immediately after the last non-empty
            // node, but it's more useful to place it after any whitespace.
            let mut j = i as usize + 1;
            while let Some(s) = before.get(j) {
                if s.ty != Ty::Space {
                    break;
                }
                offset = offset.wrapping_add(s.len);
                j += 1;
            }
            break;
        }
    }
    offset
}

/// `mapIncludes()` for the default `uniqueKeys: true`
#[derive(PartialEq, Eq, Hash)]
enum KeyId {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
}

fn key_id(node: &Node) -> Option<KeyId> {
    match &node.kind {
        NodeKind::Scalar { value, .. } => match value {
            Value::Null => Some(KeyId::Null),
            Value::Bool(b) => Some(KeyId::Bool(*b)),
            Value::Number(n) if n.is_nan() => None,
            Value::Number(n) => Some(KeyId::Num(if *n == 0.0 { 0 } else { n.to_bits() })),
            Value::String(s) => Some(KeyId::Str(s.clone())),
            _ => None,
        },
        _ => None,
    }
}

fn scalar_keys_equal(a: &Node, b: &Node) -> bool {
    match (&a.kind, &b.kind) {
        (NodeKind::Scalar { value: x, .. }, NodeKind::Scalar { value: y, .. }) => match (x, y) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(p), Value::Bool(q)) => p == q,
            (Value::Number(p), Value::Number(q)) => p == q,
            (Value::String(p), Value::String(q)) => p == q,
            _ => false,
        },
        _ => false,
    }
}

/// Keeps the scalar keys of a map under construction; linear scan for small
/// maps, a hash set once it grows.
#[derive(Default)]
struct KeyIndex {
    set: HashSet<KeyId>,
    indexed: usize,
}

impl KeyIndex {
    fn includes(&mut self, items: &[Pair], search: &Node) -> bool {
        if items.len() < 24 {
            return items.iter().any(|p| p.key.as_ref().is_some_and(|k| scalar_keys_equal(k, search)));
        }
        while self.indexed < items.len() {
            if let Some(id) = items[self.indexed].key.as_ref().and_then(key_id) {
                self.set.insert(id);
            }
            self.indexed += 1;
        }
        key_id(search).is_some_and(|id| self.set.contains(&id))
    }
}

/// Nesting limit of the composer; JS hits `RangeError: Maximum call stack size
/// exceeded` at about 1600 levels with its default stack.
const MAX_DEPTH: usize = 1500;

struct Composer<'a> {
    src: &'a str,
    doc: Option<DocB>,
    at_directives: bool,
    prelude: Vec<(usize, usize)>,
    errors: Vec<YamlError>,
    warnings: Vec<YamlError>,
    directives: Directives,
    // the composer context of compose-doc.js
    at_root: bool,
    doc_directives: Directives,
    depth: usize,
    max_depth: usize,
}

const START_COL_MSG: &str = "All mapping items must start at the same column";
const BLOCK_MSG: &str = "Block collections are not allowed within flow collections";

impl<'a> Composer<'a> {
    fn new(src: &'a str, max_depth: usize) -> Self {
        Composer {
            src,
            max_depth,
            doc: None,
            at_directives: false,
            prelude: Vec::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            directives: Directives::new(),
            at_root: true,
            doc_directives: Directives::new(),
            depth: 0,
        }
    }

    fn text(&self, s: &Src) -> &'a str {
        let src: &'a str = self.src;
        src.get(s.offset..s.offset + s.len).unwrap_or("")
    }

    fn slice(&self, off: usize, len: usize) -> &'a str {
        let src: &'a str = self.src;
        src.get(off..off + len).unwrap_or("")
    }

    fn on_error(&mut self, pos: (usize, usize), code: &str, message: impl Into<String>, warning: bool) {
        let e = YamlError { code: code.to_string(), message: message.into(), pos };
        if warning {
            self.warnings.push(e);
        } else {
            self.errors.push(e);
        }
    }

    /// `onError(number, ...)`
    fn on_error_at(&mut self, n: usize, code: &str, message: impl Into<String>, warning: bool) {
        self.on_error((n, n + 1), code, message, warning);
    }

    fn decorate(&mut self, doc: &mut DocB, after_doc: bool) {
        let (comment, after_empty_line) = self.parse_prelude();
        if !comment.is_empty() {
            if after_doc {
                doc.doc.comment = Some(match doc.doc.comment.take() {
                    Some(dc) if !dc.is_empty() => format!("{dc}\n{comment}"),
                    _ => comment,
                });
            } else if after_empty_line || doc.doc_start || doc.doc.contents.is_none() {
                doc.doc.comment_before = Some(comment);
            } else {
                let dc = doc.doc.contents.as_mut().expect("contents");
                let target: &mut Node = match &mut dc.kind {
                    NodeKind::Map { items, flow: false } if !items.is_empty() => items[0].key.as_mut().expect("pair key"),
                    NodeKind::Seq { items, flow: false } if !items.is_empty() => &mut items[0],
                    _ => dc,
                };
                target.comment_before = Some(match target.comment_before.take() {
                    Some(cb) if !cb.is_empty() => format!("{comment}\n{cb}"),
                    _ => comment,
                });
            }
        }
        if after_doc {
            doc.doc.errors.append(&mut self.errors);
            doc.doc.warnings.append(&mut self.warnings);
        } else {
            doc.doc.errors = std::mem::take(&mut self.errors);
            doc.doc.warnings = std::mem::take(&mut self.warnings);
        }
        self.prelude.clear();
        self.errors.clear();
        self.warnings.clear();
    }

    fn parse_prelude(&self) -> (String, bool) {
        let mut comment = String::new();
        let mut at_comment = false;
        let mut after_empty_line = false;
        let mut i = 0;
        while i < self.prelude.len() {
            let (off, len) = self.prelude[i];
            let source = self.slice(off, len);
            match source.as_bytes().first() {
                Some(b'#') => {
                    if !comment.is_empty() {
                        comment.push_str(if after_empty_line { "\n\n" } else { "\n" });
                    }
                    let rest = source.get(1..).unwrap_or("");
                    comment.push_str(if rest.is_empty() { " " } else { rest });
                    at_comment = true;
                    after_empty_line = false;
                }
                Some(b'%') => {
                    let next_is_comment = self.prelude.get(i + 1).is_some_and(|&(o, l)| self.slice(o, l).as_bytes().first() == Some(&b'#'));
                    if !next_is_comment {
                        i += 1;
                    }
                    at_comment = false;
                }
                _ => {
                    // This may be wrong after doc-end, but in that case it doesn't matter
                    if !at_comment {
                        after_empty_line = true;
                    }
                    at_comment = false;
                }
            }
            i += 1;
        }
        (comment, after_empty_line)
    }

    /// Advance the composer by one CST token; may yield the previous document.
    fn next(&mut self, token: &Token) -> Option<Document> {
        match token {
            Token::Directive(s) => {
                let text = self.text(s);
                let mut msgs: Vec<(usize, String, bool)> = Vec::new();
                self.directives.add(text, &mut |off, msg, warning| msgs.push((off, msg, warning)));
                for (off, msg, warning) in msgs {
                    self.on_error((s.offset + off, s.offset + s.len), "BAD_DIRECTIVE", msg, warning);
                }
                self.prelude.push((s.offset, s.len));
                self.at_directives = true;
                None
            }
            Token::Document(d) => {
                let mut doc = self.compose_doc(d);
                if self.at_directives && !doc.doc_start {
                    self.on_error(pos_tok(token), "MISSING_CHAR", "Missing directives-end/doc-start indicator line", false);
                }
                self.decorate(&mut doc, false);
                let prev = self.doc.take();
                self.doc = Some(doc);
                self.at_directives = false;
                prev.map(|p| p.doc)
            }
            Token::Src(s) => {
                match s.ty {
                    Ty::ByteOrderMark | Ty::Space => {}
                    Ty::Comment | Ty::Newline => self.prelude.push((s.offset, s.len)),
                    _ => self.on_error(pos_src(s), "UNEXPECTED_TOKEN", format!("Unsupported token {}", s.ty.name()), false),
                }
                None
            }
            Token::Error(e) => {
                let msg = if e.source.is_empty() { e.message.clone() } else { format!("{}: {}", e.message, json_quote(&e.source)) };
                let error = YamlError { code: "UNEXPECTED_TOKEN".to_string(), message: msg, pos: pos_tok(token) };
                if self.at_directives || self.doc.is_none() {
                    self.errors.push(error);
                } else if let Some(d) = self.doc.as_mut() {
                    d.doc.errors.push(error);
                }
                None
            }
            Token::DocEnd(de) => {
                if self.doc.is_none() {
                    self.on_error(pos_tok(token), "UNEXPECTED_TOKEN", "Unexpected doc-end without preceding document", false);
                    return None;
                }
                let (end_comment, end_offset) = self.resolve_end(de.end.as_deref(), de.offset + de.len, true);
                let mut doc = self.doc.take().expect("doc");
                self.decorate(&mut doc, true);
                if !end_comment.is_empty() {
                    doc.doc.comment = Some(match doc.doc.comment.take() {
                        Some(dc) if !dc.is_empty() => format!("{dc}\n{end_comment}"),
                        _ => end_comment,
                    });
                }
                doc.doc.range[2] = end_offset;
                self.doc = Some(doc);
                None
            }
            Token::Flow(_) | Token::Block(_) | Token::Map(_) | Token::Seq(_) | Token::FlowColl(_) => {
                self.on_error(pos_tok(token), "UNEXPECTED_TOKEN", format!("Unsupported token {}", token.ty().name()), false);
                None
            }
        }
    }

    /// Call at end of input to yield any remaining document.
    fn end(&mut self, force_doc: bool, end_offset: usize) -> Option<Document> {
        if let Some(mut doc) = self.doc.take() {
            self.decorate(&mut doc, true);
            return Some(doc.doc);
        }
        if force_doc {
            let _ = self.directives.at_document();
            if self.at_directives {
                self.on_error_at(end_offset, "MISSING_CHAR", "Missing directives-end indicator line", false);
            }
            let mut doc = DocB {
                doc: Document {
                    contents: None,
                    errors: Vec::new(),
                    warnings: Vec::new(),
                    comment_before: None,
                    comment: None,
                    range: [0, end_offset, end_offset],
                },
                doc_start: false,
            };
            self.decorate(&mut doc, false);
            return Some(doc.doc);
        }
        None
    }

    // ----- compose-doc.js -----

    fn compose_doc(&mut self, d: &Doc) -> DocB {
        let doc_dirs = self.directives.at_document();
        self.doc_directives = doc_dirs;
        self.at_root = true;
        self.depth = 0;
        let next = match d.value.as_deref() {
            Some(v) => Some(next_of_tok(v)),
            None => d.end.as_ref().and_then(|e| e.first()).map(next_of_src),
        };
        let props = self.resolve_props(&d.start, None, Ty::DocStart, next, d.offset, 0, true);
        let mut doc_start = false;
        if props.found.is_some() {
            doc_start = true;
            if let Some(v) = d.value.as_deref()
                && matches!(v.ty(), Ty::BlockMap | Ty::BlockSeq)
                && !props.has_newline
            {
                self.on_error_at(props.end, "MISSING_CHAR", "Block collection cannot start on same line with directives-end marker", false);
            }
        }
        let contents = match d.value.as_deref() {
            Some(v) => self.compose_node(v, &props),
            None => self.compose_empty_node(props.end, Some(&d.start), &props),
        };
        let content_end = contents.range[2];
        let (re_comment, re_offset) = self.resolve_end(d.end.as_deref(), content_end, false);
        DocB {
            doc: Document {
                contents: Some(contents),
                errors: Vec::new(),
                warnings: Vec::new(),
                comment_before: None,
                comment: non_empty(re_comment),
                range: [d.offset, content_end, re_offset],
            },
            doc_start,
        }
    }

    // ----- resolve-props.js -----

    #[allow(clippy::too_many_arguments)]
    fn resolve_props(
        &mut self,
        tokens: &[Src],
        flow: Option<&str>,
        indicator: Ty,
        next: Option<NextInfo>,
        offset: usize,
        parent_indent: usize,
        start_on_newline: bool,
    ) -> Props {
        let mut at_newline = start_on_newline;
        let mut has_space = start_on_newline;
        let mut comment = String::new();
        let mut comment_sep = String::new();
        let mut has_newline = false;
        let mut req_space = false;
        let mut tab: Option<Src> = None;
        let mut anchor: Option<Src> = None;
        let mut tag: Option<Src> = None;
        let mut newline_after_prop: Option<Src> = None;
        let mut comma: Option<Src> = None;
        let mut found: Option<Src> = None;
        let mut start: Option<usize> = None;
        for token in tokens {
            if req_space {
                if !matches!(token.ty, Ty::Space | Ty::Newline | Ty::Comma) {
                    self.on_error_at(
                        token.offset,
                        "MISSING_CHAR",
                        "Tags and anchors must be separated from the next token by white space",
                        false,
                    );
                }
                req_space = false;
            }
            if let Some(t) = tab {
                if at_newline && token.ty != Ty::Comment && token.ty != Ty::Newline {
                    self.on_error(pos_src(&t), "TAB_AS_INDENT", "Tabs are not allowed as indentation", false);
                }
                tab = None;
            }
            match token.ty {
                Ty::Space => {
                    // At the doc level, tabs at line start may be parsed
                    // as leading white space rather than indentation.
                    // In a flow collection, only the parser handles indent.
                    if flow.is_none()
                        && (indicator != Ty::DocStart || next.map(|n| n.ty) != Some(Ty::FlowCollection))
                        && self.text(token).contains('\t')
                    {
                        tab = Some(*token);
                    }
                    has_space = true;
                }
                Ty::Comment => {
                    if !has_space {
                        self.on_error(
                            pos_src(token),
                            "MISSING_CHAR",
                            "Comments must be separated from other tokens by white space characters",
                            false,
                        );
                    }
                    let text = self.text(token);
                    let cb = if text.len() > 1 { &text[1..] } else { " " };
                    if comment.is_empty() {
                        comment.push_str(cb);
                    } else {
                        comment.push_str(&comment_sep);
                        comment.push_str(cb);
                    }
                    comment_sep.clear();
                    at_newline = false;
                }
                Ty::Newline => {
                    if at_newline {
                        if !comment.is_empty() {
                            comment.push_str(self.text(token));
                        }
                    } else {
                        comment_sep.push_str(self.text(token));
                    }
                    at_newline = true;
                    has_newline = true;
                    if anchor.is_some() || tag.is_some() {
                        newline_after_prop = Some(*token);
                    }
                    has_space = true;
                }
                Ty::Anchor => {
                    if anchor.is_some() {
                        self.on_error(pos_src(token), "MULTIPLE_ANCHORS", "A node can have at most one anchor", false);
                    }
                    if self.text(token).ends_with(':') {
                        self.on_error_at(token.offset + token.len - 1, "BAD_ALIAS", "Anchor ending in : is ambiguous", true);
                    }
                    anchor = Some(*token);
                    start.get_or_insert(token.offset);
                    at_newline = false;
                    has_space = false;
                    req_space = true;
                }
                Ty::Tag => {
                    if tag.is_some() {
                        self.on_error(pos_src(token), "MULTIPLE_TAGS", "A node can have at most one tag", false);
                    }
                    tag = Some(*token);
                    start.get_or_insert(token.offset);
                    at_newline = false;
                    has_space = false;
                    req_space = true;
                }
                t if t == indicator => {
                    // Could here handle preceding comments differently
                    if anchor.is_some() || tag.is_some() {
                        let msg = format!("Anchors and tags must be after the {} indicator", self.text(token));
                        self.on_error(pos_src(token), "BAD_PROP_ORDER", msg, false);
                    }
                    if found.is_some() {
                        let msg = format!("Unexpected {} in {}", self.text(token), flow.unwrap_or("collection"));
                        self.on_error(pos_src(token), "UNEXPECTED_TOKEN", msg, false);
                    }
                    found = Some(*token);
                    at_newline = indicator == Ty::SeqItemInd || indicator == Ty::ExplicitKeyInd;
                    has_space = false;
                }
                Ty::Comma if flow.is_some() => {
                    if comma.is_some() {
                        let msg = format!("Unexpected , in {}", flow.unwrap_or(""));
                        self.on_error(pos_src(token), "UNEXPECTED_TOKEN", msg, false);
                    }
                    comma = Some(*token);
                    at_newline = false;
                    has_space = false;
                }
                _ => {
                    let msg = format!("Unexpected {} token", token.ty.name());
                    self.on_error(pos_src(token), "UNEXPECTED_TOKEN", msg, false);
                    at_newline = false;
                    has_space = false;
                }
            }
        }
        let end = tokens.last().map_or(offset, |l| l.offset + l.len);
        if req_space
            && let Some(n) = next
            && !matches!(n.ty, Ty::Space | Ty::Newline | Ty::Comma)
            && (n.ty != Ty::Scalar || !n.empty_source)
        {
            self.on_error_at(n.offset, "MISSING_CHAR", "Tags and anchors must be separated from the next token by white space", false);
        }
        if let Some(t) = tab
            && ((at_newline && t.indent <= parent_indent) || matches!(next.map(|n| n.ty), Some(Ty::BlockMap | Ty::BlockSeq)))
        {
            self.on_error(pos_src(&t), "TAB_AS_INDENT", "Tabs are not allowed as indentation", false);
        }
        Props { comma, found, comment, has_newline, anchor, tag, newline_after_prop, end, start: start.unwrap_or(end) }
    }

    // ----- resolve-end.js -----

    fn resolve_end(&mut self, end: Option<&[Src]>, mut offset: usize, req_space: bool) -> (String, usize) {
        let mut comment = String::new();
        if let Some(end) = end {
            let mut has_space = false;
            let mut sep = String::new();
            for token in end {
                match token.ty {
                    Ty::Space => has_space = true,
                    Ty::Comment => {
                        if req_space && !has_space {
                            self.on_error(
                                pos_src(token),
                                "MISSING_CHAR",
                                "Comments must be separated from other tokens by white space characters",
                                false,
                            );
                        }
                        let text = self.text(token);
                        let cb = if text.len() > 1 { &text[1..] } else { " " };
                        if comment.is_empty() {
                            comment.push_str(cb);
                        } else {
                            comment.push_str(&sep);
                            comment.push_str(cb);
                        }
                        sep.clear();
                    }
                    Ty::Newline => {
                        if !comment.is_empty() {
                            sep.push_str(self.text(token));
                        }
                        has_space = true;
                    }
                    other => {
                        self.on_error(pos_src(token), "UNEXPECTED_TOKEN", format!("Unexpected {} at node end", other.name()), false);
                    }
                }
                offset += token.len;
            }
        }
        (comment, offset)
    }

    /// `from < to - 1024` where JS offsets are UTF-16 units
    fn units_over_1024(&self, from: usize, to: usize) -> bool {
        to > from + 1024 && units_between(self.src, from, to) > 1024
    }

    fn tag_name_of(&mut self, tag_tok: &Src) -> Option<String> {
        let text = self.text(tag_tok);
        let mut msgs: Vec<String> = Vec::new();
        let res = self.doc_directives.tag_name(text, &mut |m| msgs.push(m));
        for m in msgs {
            self.on_error(pos_src(tag_tok), "TAG_RESOLVE_FAILED", m, false);
        }
        // an empty tag name (`!<>`) is falsy in JS
        res.filter(|r| !r.is_empty())
    }

    // ----- compose-node.js -----

    fn compose_node(&mut self, token: &Token, props: &Props) -> Node {
        let mut node: Option<Node> = None;
        match token {
            Token::Flow(f) if f.ty == Ty::Alias => {
                node = Some(self.compose_alias(f));
                if props.anchor.is_some() || props.tag.is_some() {
                    self.on_error(pos_tok(token), "ALIAS_PROPS", "An alias node must not specify any properties", false);
                }
            }
            Token::Flow(f) => {
                let mut n = self.compose_scalar(ScalarSrc::Flow(f), props.tag);
                if let Some(a) = &props.anchor {
                    n.anchor = Some(self.text(a).get(1..).unwrap_or("").to_string());
                }
                node = Some(n);
            }
            Token::Block(b) => {
                let mut n = self.compose_scalar(ScalarSrc::Block(b), props.tag);
                if let Some(a) = &props.anchor {
                    n.anchor = Some(self.text(a).get(1..).unwrap_or("").to_string());
                }
                node = Some(n);
            }
            Token::Map(_) | Token::Seq(_) | Token::FlowColl(_) => {
                let result = if self.depth >= self.max_depth {
                    Err("Maximum call stack size exceeded".to_string())
                } else {
                    self.depth += 1;
                    let r = self.compose_collection(token, props);
                    self.depth -= 1;
                    r
                };
                match result {
                    Ok(mut n) => {
                        if let Some(a) = &props.anchor {
                            n.anchor = Some(self.text(a).get(1..).unwrap_or("").to_string());
                        }
                        node = Some(n);
                    }
                    Err(message) => {
                        // Almost certainly here due to a stack overflow
                        self.on_error(pos_tok(token), "RESOURCE_EXHAUSTION", message, false);
                    }
                }
            }
            _ => {
                let message = match token {
                    Token::Error(e) => e.message.clone(),
                    _ => format!("Unsupported token (type: {})", token.ty().name()),
                };
                self.on_error(pos_tok(token), "UNEXPECTED_TOKEN", message, false);
            }
        }
        let mut node = match node {
            Some(n) => n,
            None => self.compose_empty_node(token.offset(), None, props),
        };
        if let Some(a) = &props.anchor
            && node.anchor.as_deref() == Some("")
        {
            self.on_error(pos_src(a), "BAD_ALIAS", "Anchor cannot be an empty string", false);
        }
        if !props.comment.is_empty() {
            if matches!(token, Token::Flow(f) if f.ty == Ty::Scalar && f.len == 0) {
                node.comment = Some(props.comment.clone());
            } else {
                node.comment_before = Some(props.comment.clone());
            }
        }
        node
    }

    fn compose_empty_node(&mut self, offset: usize, before: Option<&[Src]>, props: &Props) -> Node {
        let off = empty_scalar_position(offset, before);
        let mut node = self.compose_scalar(ScalarSrc::Empty(off), props.tag);
        if let Some(a) = &props.anchor {
            node.anchor = Some(self.text(a).get(1..).unwrap_or("").to_string());
            if node.anchor.as_deref() == Some("") {
                self.on_error(pos_src(a), "BAD_ALIAS", "Anchor cannot be an empty string", false);
            }
        }
        if !props.comment.is_empty() {
            node.comment = Some(props.comment.clone());
            node.range[2] = props.end;
        }
        node
    }

    fn compose_alias(&mut self, f: &FlowScalar) -> Node {
        let source = self.slice(f.offset, f.len).get(1..).unwrap_or("");
        if source.is_empty() {
            self.on_error_at(f.offset, "BAD_ALIAS", "Alias cannot be an empty string", false);
        }
        if source.ends_with(':') {
            self.on_error_at(f.offset + f.len - 1, "BAD_ALIAS", "Alias ending in : is ambiguous", true);
        }
        let value_end = f.offset + f.len;
        let (comment, re_offset) = self.resolve_end(f.end.as_deref(), value_end, true);
        let mut node = new_node(NodeKind::Alias { source: source.to_string() }, [f.offset, value_end, re_offset]);
        node.comment = non_empty(comment);
        node
    }

    // ----- compose-scalar.js, resolve-flow-scalar.js, resolve-block-scalar.js -----

    fn compose_scalar(&mut self, tok: ScalarSrc<'_>, tag_tok: Option<Src>) -> Node {
        let (res, plain) = match tok {
            ScalarSrc::Flow(f) => {
                let source = self.slice(f.offset, f.len);
                (self.resolve_flow_scalar(f.ty, f.offset, source, f.end.as_deref()), f.ty == Ty::Scalar)
            }
            ScalarSrc::Block(b) => (self.resolve_block_scalar(b), false),
            ScalarSrc::Empty(off) => (self.resolve_flow_scalar(Ty::Scalar, off, "", None), true),
        };
        let tag_name = tag_tok.as_ref().and_then(|t| self.tag_name_of(t));
        let tag = match (&tag_name, &tag_tok) {
            (Some(tn), Some(tt)) => self.find_scalar_tag_by_name(&res.value, tn, tt),
            _ if plain => CORE_SCALAR_TAGS.iter().copied().find(|t| t.test(&res.value)).unwrap_or(CoreTag::Str),
            _ => CoreTag::Str,
        };
        let value = tag.resolve(&res.value);
        let mut node = new_node(NodeKind::Scalar { value, style: res.ty.unwrap_or(ScalarStyle::Plain) }, res.range);
        node.tag = tag_name;
        node.comment = non_empty(res.comment);
        node
    }

    fn find_scalar_tag_by_name(&mut self, value: &str, tag_name: &str, tag_tok: &Src) -> CoreTag {
        if tag_name == "!" {
            return CoreTag::Str; // non-specific tag
        }
        let mut match_with_test: Vec<CoreTag> = Vec::new();
        for tag in CORE_SCALAR_TAGS {
            if tag.uri() == tag_name {
                if tag == CoreTag::Str {
                    return tag; // no `test`
                }
                match_with_test.push(tag);
            }
        }
        for tag in match_with_test {
            if tag.test(value) {
                return tag;
            }
        }
        if let Some(None) = known_tag(tag_name) {
            // YAML 1.1 scalar types (binary, merge, timestamp) are not ported
            return CoreTag::Str;
        }
        self.on_error(pos_src(tag_tok), "TAG_RESOLVE_FAILED", format!("Unresolved tag: {tag_name}"), tag_name != STR_TAG);
        CoreTag::Str
    }

    fn resolve_flow_scalar(&mut self, ty: Ty, offset: usize, source: &str, end: Option<&[Src]>) -> Resolved {
        let (style, value) = match ty {
            Ty::Scalar => (ScalarStyle::Plain, self.plain_value(source, offset)),
            Ty::SingleQuotedScalar => (ScalarStyle::QuoteSingle, self.single_quoted_value(source, offset)),
            Ty::DoubleQuotedScalar => (ScalarStyle::QuoteDouble, self.double_quoted_value(source, offset)),
            _ => {
                self.on_error(
                    (offset, offset + source.len()),
                    "UNEXPECTED_TOKEN",
                    format!("Expected a flow scalar value, but found: {}", ty.name()),
                    false,
                );
                return Resolved {
                    value: String::new(),
                    ty: None,
                    comment: String::new(),
                    range: [offset, offset + source.len(), offset + source.len()],
                };
            }
        };
        let value_end = offset + source.len();
        let (comment, re_offset) = self.resolve_end(end, value_end, true);
        Resolved { value, ty: Some(style), comment, range: [offset, value_end, re_offset] }
    }

    fn plain_value(&mut self, source: &str, offset: usize) -> String {
        let bad_char = match source.as_bytes().first() {
            Some(b'\t') => "a tab character".to_string(),
            Some(b',') => "flow indicator character ,".to_string(),
            Some(b'%') => "directive indicator character %".to_string(),
            Some(c @ (b'|' | b'>')) => format!("block scalar indicator {}", *c as char),
            Some(c @ (b'@' | b'`')) => format!("reserved character {}", *c as char),
            _ => String::new(),
        };
        if !bad_char.is_empty() {
            self.on_error_at(offset, "BAD_SCALAR_START", format!("Plain value cannot start with {bad_char}"), false);
        }
        unfold_lines(source)
    }

    fn single_quoted_value(&mut self, source: &str, offset: usize) -> String {
        if !source.ends_with('\'') || source.len() == 1 {
            self.on_error_at(offset + source.len(), "MISSING_CHAR", "Missing closing 'quote", false);
        }
        let inner = if source.len() <= 1 {
            ""
        } else {
            let last = source.chars().next_back().map_or(1, char::len_utf8);
            source.get(1..source.len() - last).unwrap_or("")
        };
        unfold_lines(inner).replace("''", "'")
    }

    fn double_quoted_value(&mut self, source: &str, offset: usize) -> String {
        let b = source.as_bytes();
        let last_len = source.chars().next_back().map_or(0, char::len_utf8);
        let limit = b.len().saturating_sub(last_len);
        let mut res: Vec<u8> = Vec::with_capacity(b.len());
        let mut i = 1;
        while i < limit {
            let ch = b[i];
            if ch == b'\r' && b.get(i + 1) == Some(&b'\n') {
                i += 1;
                continue;
            }
            if ch == b'\n' {
                let (fold, off) = fold_newline(b, i);
                res.extend_from_slice(fold.as_bytes());
                i = off;
            } else if ch == b'\\' {
                i += 1;
                let next = b.get(i).copied();
                let cc: Option<&str> = match next {
                    Some(b'0') => Some("\0"),
                    Some(b'a') => Some("\x07"),
                    Some(b'b') => Some("\x08"),
                    Some(b'e') => Some("\x1b"),
                    Some(b'f') => Some("\x0c"),
                    Some(b'n') => Some("\n"),
                    Some(b'r') => Some("\r"),
                    Some(b't') => Some("\t"),
                    Some(b'v') => Some("\x0b"),
                    Some(b'N') => Some("\u{85}"),
                    Some(b'_') => Some("\u{a0}"),
                    Some(b'L') => Some("\u{2028}"),
                    Some(b'P') => Some("\u{2029}"),
                    Some(b' ') => Some(" "),
                    Some(b'"') => Some("\""),
                    Some(b'/') => Some("/"),
                    Some(b'\\') => Some("\\"),
                    Some(b'\t') => Some("\t"),
                    _ => None,
                };
                if let Some(cc) = cc {
                    res.extend_from_slice(cc.as_bytes());
                } else if next == Some(b'\n') {
                    // skip escaped newlines, but still trim the following line
                    let mut n = b.get(i + 1);
                    while n == Some(&b' ') || n == Some(&b'\t') {
                        i += 1;
                        n = b.get(i + 1);
                    }
                } else if next == Some(b'\r') && b.get(i + 1) == Some(&b'\n') {
                    // skip escaped CRLF newlines, but still trim the following line
                    i += 1;
                    let mut n = b.get(i + 1);
                    while n == Some(&b' ') || n == Some(&b'\t') {
                        i += 1;
                        n = b.get(i + 1);
                    }
                } else if matches!(next, Some(b'x' | b'u' | b'U')) {
                    let length = match next {
                        Some(b'x') => 2,
                        Some(b'u') => 4,
                        _ => 8,
                    };
                    let consumed = self.parse_char_code(source, i + 1, length, offset, &mut res);
                    i = consumed.saturating_sub(1).max(i);
                } else {
                    // raw = source.substr(i - 1, 2)
                    let clen = source.get(i..).and_then(|s| s.chars().next()).map_or(1, char::len_utf8);
                    let raw = source.get(i - 1..i + clen).unwrap_or("\\");
                    self.on_error_at(offset + i - 1, "BAD_DQ_ESCAPE", format!("Invalid escape sequence {raw}"), false);
                    res.extend_from_slice(raw.as_bytes());
                    i += clen - 1;
                }
            } else if ch == b' ' || ch == b'\t' {
                // trim trailing whitespace
                let ws_start = i;
                let mut next = b.get(i + 1);
                while next == Some(&b' ') || next == Some(&b'\t') {
                    i += 1;
                    next = b.get(i + 1);
                }
                if next != Some(&b'\n') && !(next == Some(&b'\r') && b.get(i + 2) == Some(&b'\n')) {
                    res.extend_from_slice(&b[ws_start..=i]);
                }
            } else {
                res.push(ch);
            }
            i += 1;
        }
        if !source.ends_with('"') || source.len() == 1 {
            self.on_error_at(offset + source.len(), "MISSING_CHAR", "Missing closing \"quote", false);
        }
        String::from_utf8(res).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    /// `parseCharCode`: appends the decoded char to `res` and returns the byte
    /// index just past the consumed `length` UTF-16 units (JS: `i += length`).
    fn parse_char_code(&mut self, source: &str, at: usize, length: usize, offset: usize, res: &mut Vec<u8>) -> usize {
        let end = advance_units(source, at, length);
        let cc = source.get(at..end).unwrap_or("");
        let ok = utf16_len(cc) == length && cc.bytes().all(|c| c.is_ascii_hexdigit());
        let code = if ok { u32::from_str_radix(cc, 16).ok() } else { None };
        match code {
            Some(c) if (0xd800..0xdc00).contains(&c) => {
                // a high surrogate escape directly followed by a low surrogate escape
                if let Some((low, consumed_to)) = low_surrogate_escape(source, end) {
                    let ch = 0x10000 + ((c - 0xd800) << 10) + (low - 0xdc00);
                    if let Some(ch) = char::from_u32(ch) {
                        let mut buf = [0u8; 4];
                        res.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        return consumed_to;
                    }
                }
                res.extend_from_slice("\u{fffd}".as_bytes());
                end
            }
            Some(c) => {
                match char::from_u32(c) {
                    Some(ch) => {
                        let mut buf = [0u8; 4];
                        res.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                    None if c <= 0x10ffff => {
                        // lone low surrogate: not representable in a Rust string
                        res.extend_from_slice("\u{fffd}".as_bytes());
                    }
                    None => {
                        return self.bad_char_code(source, at, length, offset, res);
                    }
                }
                end
            }
            None => self.bad_char_code(source, at, length, offset, res),
        }
    }

    fn bad_char_code(&mut self, source: &str, at: usize, length: usize, offset: usize, res: &mut Vec<u8>) -> usize {
        // raw = source.substr(offset - 2, length + 2)
        let start = at - 2;
        let raw_end = advance_units(source, start, length + 2);
        let raw = source.get(start..raw_end).unwrap_or("");
        self.on_error_at(offset + start, "BAD_DQ_ESCAPE", format!("Invalid escape sequence {raw}"), false);
        res.extend_from_slice(raw.as_bytes());
        advance_units(source, at, length)
    }

    fn resolve_block_scalar(&mut self, scalar: &BlockScalar) -> Resolved {
        let start = scalar.offset;
        let strict = true;
        let Some(header) = self.parse_block_scalar_header(scalar, strict) else {
            return Resolved { value: String::new(), ty: None, comment: String::new(), range: [start, start, start] };
        };
        let ty = if header.mode == b'>' { ScalarStyle::BlockFolded } else { ScalarStyle::BlockLiteral };
        let source = self.slice(scalar.src_off, scalar.src_len);
        let lines: Vec<(&str, &str)> = if source.is_empty() { Vec::new() } else { split_lines(source) };

        // determine the end of content & start of chomping
        let mut chomp_start = lines.len();
        for i in (0..lines.len()).rev() {
            let content = lines[i].1;
            if content.is_empty() || content == "\r" {
                chomp_start = i;
            } else {
                break;
            }
        }

        // shortcut for empty contents
        if chomp_start == 0 {
            let value =
                if header.chomp == b'+' && !lines.is_empty() { "\n".repeat(std::cmp::max(1, lines.len() - 1)) } else { String::new() };
            let mut end = start + header.length;
            if !source.is_empty() {
                end += source.len();
            }
            return Resolved { value, ty: Some(ty), comment: header.comment, range: [start, end, end] };
        }

        // find the indentation level to trim from start
        let mut trim_indent = scalar.indent + header.indent;
        let mut offset = scalar.offset + header.length;
        let mut content_start = 0;
        for (i, &(indent, content)) in lines.iter().enumerate().take(chomp_start) {
            if content.is_empty() || content == "\r" {
                if header.indent == 0 && indent.len() > trim_indent {
                    trim_indent = indent.len();
                }
            } else {
                if indent.len() < trim_indent {
                    let message = "Block scalars with more-indented leading empty lines must use an explicit indentation indicator";
                    self.on_error_at(offset + indent.len(), "MISSING_CHAR", message, false);
                }
                if header.indent == 0 {
                    trim_indent = indent.len();
                }
                content_start = i;
                if trim_indent == 0 && !self.at_root {
                    let message = "Block scalar values in collections must be indented";
                    self.on_error_at(offset, "BAD_INDENT", message, false);
                }
                break;
            }
            offset += indent.len() + content.len() + 1;
        }

        // include trailing more-indented empty lines in content
        // (`for (let i = lines.length - 1; i >= chompStart; --i)` re-reads `chompStart`)
        let mut i = lines.len();
        while i > chomp_start {
            i -= 1;
            if lines[i].0.len() > trim_indent {
                chomp_start = i + 1;
            }
        }

        let mut value = String::with_capacity(source.len());
        let mut sep = "";
        let mut prev_more_indented = false;
        // leading whitespace is kept intact
        for &(indent, _) in lines.iter().take(content_start) {
            value.push_str(indent.get(trim_indent..).unwrap_or(""));
            value.push('\n');
        }
        for &(indent0, content0) in &lines[content_start..chomp_start] {
            let (mut indent, mut content) = (indent0, content0);
            offset += indent.len() + content.len() + 1;
            let crlf = content.ends_with('\r');
            if crlf {
                content = &content[..content.len() - 1];
            }
            if !content.is_empty() && indent.len() < trim_indent {
                let src = if header.indent != 0 { "explicit indentation indicator" } else { "first line" };
                let message = format!("Block scalar lines must not be less indented than their {src}");
                let at = (offset as isize - content.len() as isize - if crlf { 2 } else { 1 }).max(0) as usize;
                self.on_error_at(at, "BAD_INDENT", message, false);
                indent = "";
            }
            if ty == ScalarStyle::BlockLiteral {
                value.push_str(sep);
                value.push_str(indent.get(trim_indent..).unwrap_or(""));
                value.push_str(content);
                sep = "\n";
            } else if indent.len() > trim_indent || content.starts_with('\t') {
                // more-indented content within a folded block
                if sep == " " {
                    sep = "\n";
                } else if !prev_more_indented && sep == "\n" {
                    sep = "\n\n";
                }
                value.push_str(sep);
                value.push_str(indent.get(trim_indent..).unwrap_or(""));
                value.push_str(content);
                sep = "\n";
                prev_more_indented = true;
            } else if content.is_empty() {
                // empty line
                if sep == "\n" {
                    value.push('\n');
                } else {
                    sep = "\n";
                }
            } else {
                value.push_str(sep);
                value.push_str(content);
                sep = " ";
                prev_more_indented = false;
            }
        }
        match header.chomp {
            b'-' => {}
            b'+' => {
                for &(indent, _) in lines.iter().skip(chomp_start) {
                    value.push('\n');
                    value.push_str(indent.get(trim_indent..).unwrap_or(""));
                }
                if !value.ends_with('\n') {
                    value.push('\n');
                }
            }
            _ => value.push('\n'),
        }
        let end = start + header.length + source.len();
        Resolved { value, ty: Some(ty), comment: header.comment, range: [start, end, end] }
    }

    fn parse_block_scalar_header(&mut self, scalar: &BlockScalar, strict: bool) -> Option<BlockHeader> {
        let offset = scalar.offset;
        let first = scalar.props.first();
        let h = match first {
            Some(Token::Src(h)) if h.ty == Ty::BlockScalarHeader => *h,
            _ => {
                let pos = first.map_or((offset, offset + 1), pos_tok);
                self.on_error(pos, "IMPOSSIBLE", "Block scalar header not found", false);
                return None;
            }
        };
        let source = self.text(&h);
        let mode = source.as_bytes()[0];
        let mut indent = 0usize;
        let mut chomp = 0u8;
        let mut error: Option<usize> = None;
        for (i, &ch) in source.as_bytes().iter().enumerate().skip(1) {
            if chomp == 0 && (ch == b'-' || ch == b'+') {
                chomp = ch;
            } else {
                let n = if ch.is_ascii_digit() { usize::from(ch - b'0') } else { 0 };
                if indent == 0 && n != 0 {
                    indent = n;
                } else if error.is_none() {
                    error = Some(offset + i);
                }
            }
        }
        if let Some(e) = error {
            self.on_error_at(e, "UNEXPECTED_TOKEN", format!("Block scalar header includes extra characters: {source}"), false);
        }
        let mut has_space = false;
        let mut comment = String::new();
        let mut length = source.len();
        for token in scalar.props.iter().skip(1) {
            match token {
                Token::Src(s) if matches!(s.ty, Ty::Space | Ty::Newline) => {
                    if s.ty == Ty::Space {
                        has_space = true;
                    }
                    length += s.len;
                }
                Token::Src(s) if s.ty == Ty::Comment => {
                    if strict && !has_space {
                        self.on_error(
                            pos_src(s),
                            "MISSING_CHAR",
                            "Comments must be separated from other tokens by white space characters",
                            false,
                        );
                    }
                    length += s.len;
                    comment = self.text(s).get(1..).unwrap_or("").to_string();
                }
                Token::Error(e) => {
                    self.on_error(pos_tok(token), "UNEXPECTED_TOKEN", e.message.clone(), false);
                    length += e.len;
                }
                other => {
                    let message = format!("Unexpected token in block scalar header: {}", other.ty().name());
                    self.on_error(pos_tok(other), "UNEXPECTED_TOKEN", message, false);
                    if let Token::Src(s) = other {
                        length += s.len;
                    }
                }
            }
        }
        Some(BlockHeader { mode, indent, chomp, comment, length })
    }

    // ----- compose-collection.js -----

    fn compose_collection(&mut self, token: &Token, props: &Props) -> Result<Node, String> {
        let tag_tok = props.tag;
        let tag_name = match &tag_tok {
            None => None,
            Some(t) => self.tag_name_of(t),
        };
        if matches!(token, Token::Seq(_)) {
            let last_prop = match (&props.anchor, &tag_tok) {
                (Some(a), Some(t)) => Some(if a.offset > t.offset { a } else { t }),
                (a, t) => a.as_ref().or(t.as_ref()),
            };
            if let Some(lp) = last_prop
                && props.newline_after_prop.as_ref().is_none_or(|nl| nl.offset < lp.offset)
            {
                let lp = *lp;
                self.on_error(pos_src(&lp), "MISSING_CHAR", "Missing newline after block sequence props", false);
            }
        }
        let exp_map = match token {
            Token::Map(_) => true,
            Token::Seq(_) => false,
            Token::FlowColl(fc) => fc.start.ty == Ty::FlowMapStart,
            _ => false,
        };
        let exp_type = if exp_map { "map" } else { "seq" };
        // shortcut: check if it's a generic YAMLMap or YAMLSeq
        // before jumping into the custom tag logic.
        let generic = match (&tag_tok, &tag_name) {
            (Some(_), Some(tn)) => tn == "!" || (tn == MAP_TAG && exp_map) || (tn == SEQ_TAG && !exp_map),
            _ => true,
        };
        if !generic && let (Some(tt), Some(tn)) = (&tag_tok, &tag_name) {
            // Not a tag of the core schema: knownTags (YAML 1.1 types are not ported)
            match known_tag(tn) {
                Some(kind) if kind == Some(exp_type) => {}
                Some(kind) => {
                    let msg = format!("{} used for {} collection, but expects {}", tn, exp_type, kind.unwrap_or("scalar"));
                    self.on_error(pos_src(tt), "BAD_COLLECTION_TYPE", msg, true);
                }
                None => {
                    self.on_error(pos_src(tt), "TAG_RESOLVE_FAILED", format!("Unresolved tag: {tn}"), true);
                }
            }
        }
        self.resolve_collection(token, tag_name.as_deref())
    }

    fn resolve_collection(&mut self, token: &Token, tag_name: Option<&str>) -> Result<Node, String> {
        let (mut coll, coll_tag) = match token {
            Token::Map(bm) => (self.resolve_block_map(bm), MAP_TAG),
            Token::Seq(bs) => (self.resolve_block_seq(bs), SEQ_TAG),
            Token::FlowColl(fc) => {
                let is_map = fc.start.ty == Ty::FlowMapStart;
                (self.resolve_flow_collection(fc)?, if is_map { MAP_TAG } else { SEQ_TAG })
            }
            _ => unreachable!("collection token expected"),
        };
        // If we got a tagName matching the class, or the tag name is '!',
        // then use the tagName from the node class used to create it.
        match tag_name {
            Some("!") => coll.tag = Some(coll_tag.to_string()),
            Some(tn) if tn == coll_tag => coll.tag = Some(coll_tag.to_string()),
            Some(tn) => coll.tag = Some(tn.to_string()),
            None => {}
        }
        Ok(coll)
    }

    // ----- resolve-block-map.js -----

    fn resolve_block_map(&mut self, bm: &BlockMap) -> Node {
        if self.at_root {
            self.at_root = false;
        }
        let mut items: Vec<Pair> = Vec::new();
        let mut map_comment: Option<String> = None;
        let mut keys = KeyIndex::default();
        let mut offset = bm.offset;
        let mut comment_end: Option<usize> = None;
        for coll_item in &bm.items {
            let Item { start, key, sep, value, .. } = coll_item;
            // key properties
            let key_next = match key.as_deref() {
                Some(k) => Some(next_of_tok(k)),
                None => sep.as_ref().and_then(|s| s.first()).map(next_of_src),
            };
            let key_props = self.resolve_props(start, None, Ty::ExplicitKeyInd, key_next, offset, bm.indent, true);
            let implicit_key = key_props.found.is_none();
            if implicit_key {
                if let Some(k) = key.as_deref() {
                    if k.ty() == Ty::BlockSeq {
                        self.on_error_at(offset, "BLOCK_AS_IMPLICIT_KEY", "A block sequence may not be used as an implicit map key", false);
                    } else if k.indent().is_some_and(|i| i != bm.indent) {
                        self.on_error_at(offset, "BAD_INDENT", START_COL_MSG, false);
                    }
                }
                if key_props.anchor.is_none() && key_props.tag.is_none() && sep.is_none() {
                    comment_end = Some(key_props.end);
                    if !key_props.comment.is_empty() {
                        match &mut map_comment {
                            Some(c) if !c.is_empty() => {
                                c.push('\n');
                                c.push_str(&key_props.comment);
                            }
                            _ => map_comment = Some(key_props.comment.clone()),
                        }
                    }
                    continue;
                }
                if key_props.newline_after_prop.is_some() || contains_newline(self.src, key.as_deref()) {
                    let pos = match key.as_deref() {
                        Some(k) => pos_tok(k),
                        None => start.last().map_or((0, 1), pos_src),
                    };
                    self.on_error(pos, "MULTILINE_IMPLICIT_KEY", "Implicit keys need to be on a single line", false);
                }
            } else if key_props.found.map(|f| f.indent) != Some(bm.indent) {
                self.on_error_at(offset, "BAD_INDENT", START_COL_MSG, false);
            }
            // key value
            let key_start = key_props.end;
            let mut key_node = match key.as_deref() {
                Some(k) => self.compose_node(k, &key_props),
                None => self.compose_empty_node(key_start, Some(start), &key_props),
            };
            if keys.includes(&items, &key_node) {
                self.on_error_at(key_start, "DUPLICATE_KEY", "Map keys must be unique", false);
            }
            // value properties
            let value_next = value.as_deref().map(next_of_tok);
            let value_props = self.resolve_props(
                sep.as_deref().unwrap_or(&[]),
                None,
                Ty::MapValueInd,
                value_next,
                key_node.range[2],
                bm.indent,
                key.as_deref().is_none_or(|k| k.ty() == Ty::BlockScalar),
            );
            offset = value_props.end;
            if let Some(found) = value_props.found {
                if implicit_key {
                    if value.as_deref().map(Token::ty) == Some(Ty::BlockMap) && !value_props.has_newline {
                        self.on_error_at(offset, "BLOCK_AS_IMPLICIT_KEY", "Nested mappings are not allowed in compact mappings", false);
                    }
                    if self.units_over_1024(key_props.start, found.offset) {
                        self.on_error(
                            (key_node.range[0], key_node.range[1]),
                            "KEY_OVER_1024_CHARS",
                            "The : indicator must be at most 1024 chars after the start of an implicit block mapping key",
                            false,
                        );
                    }
                }
                // value value
                let value_node = match value.as_deref() {
                    Some(v) => self.compose_node(v, &value_props),
                    None => self.compose_empty_node(offset, sep.as_deref(), &value_props),
                };
                offset = value_node.range[2];
                items.push(Pair { key: Some(key_node), value: Some(value_node) });
            } else {
                // key with no value
                if implicit_key {
                    self.on_error(
                        (key_node.range[0], key_node.range[1]),
                        "MISSING_CHAR",
                        "Implicit map keys need to be followed by map values",
                        false,
                    );
                }
                if !value_props.comment.is_empty() {
                    match &mut key_node.comment {
                        Some(c) if !c.is_empty() => {
                            c.push('\n');
                            c.push_str(&value_props.comment);
                        }
                        _ => key_node.comment = Some(value_props.comment.clone()),
                    }
                }
                items.push(Pair { key: Some(key_node), value: None });
            }
        }
        if let Some(ce) = comment_end
            && ce != 0
            && ce < offset
        {
            self.on_error_at(ce, "IMPOSSIBLE", "Map comment with trailing content", false);
        }
        let mut node = new_node(NodeKind::Map { items, flow: false }, [bm.offset, offset, comment_end.unwrap_or(offset)]);
        node.comment = map_comment;
        node
    }

    // ----- resolve-block-seq.js -----

    fn resolve_block_seq(&mut self, bs: &BlockSeq) -> Node {
        if self.at_root {
            self.at_root = false;
        }
        let mut items: Vec<Node> = Vec::new();
        let mut seq_comment: Option<String> = None;
        let mut offset = bs.offset;
        let mut comment_end: Option<usize> = None;
        for Item { start, value, .. } in &bs.items {
            let props = self.resolve_props(start, None, Ty::SeqItemInd, value.as_deref().map(next_of_tok), offset, bs.indent, true);
            if props.found.is_none() {
                if props.anchor.is_some() || props.tag.is_some() || value.is_some() {
                    if value.as_deref().map(Token::ty) == Some(Ty::BlockSeq) {
                        self.on_error_at(props.end, "BAD_INDENT", "All sequence items must start at the same column", false);
                    } else {
                        self.on_error_at(offset, "MISSING_CHAR", "Sequence item without - indicator", false);
                    }
                } else {
                    comment_end = Some(props.end);
                    if !props.comment.is_empty() {
                        seq_comment = Some(props.comment.clone());
                    }
                    continue;
                }
            }
            let node = match value.as_deref() {
                Some(v) => self.compose_node(v, &props),
                None => self.compose_empty_node(props.end, Some(start), &props),
            };
            offset = node.range[2];
            items.push(node);
        }
        let mut node = new_node(NodeKind::Seq { items, flow: false }, [bs.offset, offset, comment_end.unwrap_or(offset)]);
        node.comment = seq_comment;
        node
    }

    // ----- resolve-flow-collection.js -----

    fn resolve_flow_collection(&mut self, fc: &FlowColl) -> Result<Node, String> {
        let is_map = fc.start.ty == Ty::FlowMapStart;
        let fc_name = if is_map { "flow map" } else { "flow sequence" };
        let mut pairs: Vec<Pair> = Vec::new();
        let mut nodes: Vec<Node> = Vec::new();
        let mut coll_comment: Option<String> = None;
        let mut keys = KeyIndex::default();
        let at_root = self.at_root;
        if at_root {
            self.at_root = false;
        }
        let mut offset = fc.offset + fc.start.len;
        for (i, coll_item) in fc.items.iter().enumerate() {
            let Item { start, key, sep, value, .. } = coll_item;
            let key_next = match key.as_deref() {
                Some(k) => Some(next_of_tok(k)),
                None => sep.as_ref().and_then(|s| s.first()).map(next_of_src),
            };
            let mut props = self.resolve_props(start, Some(fc_name), Ty::ExplicitKeyInd, key_next, offset, fc.indent, false);
            if props.found.is_none() {
                if props.anchor.is_none() && props.tag.is_none() && sep.is_none() && value.is_none() {
                    if let (0, Some(c)) = (i, props.comma) {
                        self.on_error(pos_src(&c), "UNEXPECTED_TOKEN", format!("Unexpected , in {fc_name}"), false);
                    } else if i + 1 < fc.items.len() {
                        self.on_error_at(props.start, "UNEXPECTED_TOKEN", format!("Unexpected empty item in {fc_name}"), false);
                    }
                    if !props.comment.is_empty() {
                        match &mut coll_comment {
                            Some(c) if !c.is_empty() => {
                                c.push('\n');
                                c.push_str(&props.comment);
                            }
                            _ => coll_comment = Some(props.comment.clone()),
                        }
                    }
                    offset = props.end;
                    continue;
                }
                if !is_map && contains_newline(self.src, key.as_deref()) {
                    let pos = key.as_deref().map_or((0, 1), pos_tok);
                    self.on_error(pos, "MULTILINE_IMPLICIT_KEY", "Implicit keys of flow sequence pairs need to be on a single line", false);
                }
            }
            if i == 0 {
                if let Some(c) = props.comma {
                    self.on_error(pos_src(&c), "UNEXPECTED_TOKEN", format!("Unexpected , in {fc_name}"), false);
                }
            } else {
                if props.comma.is_none() {
                    self.on_error_at(props.start, "MISSING_CHAR", format!("Missing , between {fc_name} items"), false);
                }
                if !props.comment.is_empty() {
                    let mut prev_item_comment = "";
                    for st in start {
                        match st.ty {
                            Ty::Comma | Ty::Space => {}
                            Ty::Comment => {
                                prev_item_comment = self.text(st).get(1..).unwrap_or("");
                                break;
                            }
                            _ => break,
                        }
                    }
                    if !prev_item_comment.is_empty() {
                        let prev: Option<&mut Node> = if is_map {
                            pairs.last_mut().and_then(|p| if p.value.is_some() { p.value.as_mut() } else { p.key.as_mut() })
                        } else {
                            nodes.last_mut()
                        };
                        let Some(prev) = prev else {
                            return Err("Cannot read properties of undefined (reading 'comment')".to_string());
                        };
                        match &mut prev.comment {
                            Some(c) if !c.is_empty() => {
                                c.push('\n');
                                c.push_str(prev_item_comment);
                            }
                            _ => prev.comment = Some(prev_item_comment.to_string()),
                        }
                        let skip = prev_item_comment.len() + 1;
                        props.comment = props.comment.get(skip..).unwrap_or("").to_string();
                    }
                }
            }
            if !is_map && sep.is_none() && props.found.is_none() {
                // item is a value in a seq
                // -> key & sep are empty, start does not include ? or :
                let value_node = match value.as_deref() {
                    Some(v) => self.compose_node(v, &props),
                    None => self.compose_empty_node(props.end, None, &props),
                };
                offset = value_node.range[2];
                let value_range = (value_node.range[0], value_node.range[1]);
                nodes.push(value_node);
                if value.as_deref().is_some_and(is_block) {
                    self.on_error(value_range, "BLOCK_IN_FLOW", BLOCK_MSG, false);
                }
            } else {
                // item is a key+value pair
                // key value
                let key_start = props.end;
                let key_node = match key.as_deref() {
                    Some(k) => self.compose_node(k, &props),
                    None => self.compose_empty_node(key_start, Some(start), &props),
                };
                if key.as_deref().is_some_and(is_block) {
                    self.on_error((key_node.range[0], key_node.range[1]), "BLOCK_IN_FLOW", BLOCK_MSG, false);
                }
                // value properties
                let value_next = value.as_deref().map(next_of_tok);
                let value_props = self.resolve_props(
                    sep.as_deref().unwrap_or(&[]),
                    Some(fc_name),
                    Ty::MapValueInd,
                    value_next,
                    key_node.range[2],
                    fc.indent,
                    false,
                );
                if let Some(found) = value_props.found {
                    if !is_map && props.found.is_none() {
                        if let Some(sep) = sep {
                            for st in sep {
                                if st.offset == found.offset {
                                    break;
                                }
                                if st.ty == Ty::Newline {
                                    self.on_error(
                                        pos_src(st),
                                        "MULTILINE_IMPLICIT_KEY",
                                        "Implicit keys of flow sequence pairs need to be on a single line",
                                        false,
                                    );
                                    break;
                                }
                            }
                        }
                        if self.units_over_1024(props.start, found.offset) {
                            self.on_error(
                                pos_src(&found),
                                "KEY_OVER_1024_CHARS",
                                "The : indicator must be at most 1024 chars after the start of an implicit flow sequence key",
                                false,
                            );
                        }
                    }
                } else if let Some(v) = value.as_deref() {
                    // `'source' in value && value.source?.[0] === ':'`
                    let starts_with_colon = match v {
                        Token::Flow(f) => self.slice(f.offset, f.len).starts_with(':'),
                        Token::Src(s) | Token::Directive(s) => self.text(s).starts_with(':'),
                        Token::Error(e) => e.source.starts_with(':'),
                        Token::Block(b) => self.slice(b.src_off, b.src_len).starts_with(':'),
                        Token::DocEnd(_) | Token::Document(_) | Token::Map(_) | Token::Seq(_) | Token::FlowColl(_) => false,
                    };
                    if starts_with_colon {
                        self.on_error(pos_tok(v), "MISSING_CHAR", format!("Missing space after : in {fc_name}"), false);
                    } else {
                        self.on_error_at(value_props.start, "MISSING_CHAR", format!("Missing , or : between {fc_name} items"), false);
                    }
                }
                // value value
                let mut key_node = key_node;
                let value_node = match value.as_deref() {
                    Some(v) => Some(self.compose_node(v, &value_props)),
                    None if value_props.found.is_some() => Some(self.compose_empty_node(value_props.end, sep.as_deref(), &value_props)),
                    None => None,
                };
                match &value_node {
                    Some(vn) => {
                        if value.as_deref().is_some_and(is_block) {
                            self.on_error((vn.range[0], vn.range[1]), "BLOCK_IN_FLOW", BLOCK_MSG, false);
                        }
                    }
                    None => {
                        if !value_props.comment.is_empty() {
                            match &mut key_node.comment {
                                Some(c) if !c.is_empty() => {
                                    c.push('\n');
                                    c.push_str(&value_props.comment);
                                }
                                _ => key_node.comment = Some(value_props.comment.clone()),
                            }
                        }
                    }
                }
                let next_offset = match &value_node {
                    Some(vn) => vn.range[2],
                    None => value_props.end,
                };
                if is_map {
                    if keys.includes(&pairs, &key_node) {
                        self.on_error_at(key_start, "DUPLICATE_KEY", "Map keys must be unique", false);
                    }
                    pairs.push(Pair { key: Some(key_node), value: value_node });
                } else {
                    let end_range = match &value_node {
                        Some(vn) => vn.range,
                        None => key_node.range,
                    };
                    let range = [key_node.range[0], end_range[1], end_range[2]];
                    nodes.push(new_node(NodeKind::Map { items: vec![Pair { key: Some(key_node), value: value_node }], flow: true }, range));
                }
                offset = next_offset;
            }
        }
        let expected_end = if is_map { "}" } else { "]" };
        let mut end_iter = fc.end.iter();
        let ce = end_iter.next();
        let mut ee: Vec<Src> = end_iter.copied().collect();
        let mut ce_pos = offset;
        if ce.is_some_and(|c| self.text(c) == expected_end) {
            let c = ce.expect("ce");
            ce_pos = c.offset + c.len;
        } else {
            let name = if is_map { "Flow map" } else { "Flow sequence" };
            let msg = if at_root {
                format!("{name} must end with a {expected_end}")
            } else {
                format!("{name} in block collection must be sufficiently indented and end with a {expected_end}")
            };
            self.on_error_at(offset, if at_root { "MISSING_CHAR" } else { "BAD_INDENT" }, msg, false);
            if let Some(c) = ce
                && utf16_len(self.text(c)) != 1
            {
                ee.insert(0, *c);
            }
        }
        let range;
        if !ee.is_empty() {
            let (end_comment, end_offset) = self.resolve_end(Some(&ee), ce_pos, true);
            if !end_comment.is_empty() {
                match &mut coll_comment {
                    Some(c) if !c.is_empty() => {
                        c.push('\n');
                        c.push_str(&end_comment);
                    }
                    _ => coll_comment = Some(end_comment),
                }
            }
            range = [fc.offset, ce_pos, end_offset];
        } else {
            range = [fc.offset, ce_pos, ce_pos];
        }
        let kind = if is_map { NodeKind::Map { items: pairs, flow: true } } else { NodeKind::Seq { items: nodes, flow: true } };
        let mut node = new_node(kind, range);
        node.comment = coll_comment;
        Ok(node)
    }
}

fn is_block(t: &Token) -> bool {
    matches!(t.ty(), Ty::BlockMap | Ty::BlockSeq)
}

enum ScalarSrc<'t> {
    Flow(&'t FlowScalar),
    Block(&'t BlockScalar),
    /// synthetic empty plain scalar at this offset
    Empty(usize),
}

struct BlockHeader {
    mode: u8,
    indent: usize,
    chomp: u8,
    comment: String,
    length: usize,
}

/// `source.split(/\n( *)/)` as `[indent, content]` lines
fn split_lines(source: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let first_end = source.find('\n').unwrap_or(source.len());
    let first = &source[..first_end];
    let ind = first.bytes().take_while(|&c| c == b' ').count();
    lines.push((&first[..ind], &first[ind..]));
    let mut pos = first_end;
    while pos < source.len() {
        // source[pos] == '\n'
        let rest = &source[pos + 1..];
        let ind = rest.bytes().take_while(|&c| c == b' ').count();
        let after = &rest[ind..];
        let content_end = after.find('\n').unwrap_or(after.len());
        lines.push((&rest[..ind], &after[..content_end]));
        pos = pos + 1 + ind + content_end;
    }
    lines
}

fn trim_end_ws(s: &str) -> &str {
    s.trim_end_matches([' ', '\t'])
}

fn trim_both_ws(s: &str) -> &str {
    s.trim_matches([' ', '\t'])
}

/// `unfoldLines()`
fn unfold_lines(source: &str) -> String {
    let b = source.as_bytes();
    let Some(first_nl) = source.find('\n') else {
        return source.to_string();
    };
    let line_end = |nl: usize, from: usize| -> usize { if nl > from && b[nl - 1] == b'\r' { nl - 1 } else { nl } };
    let mut res = String::with_capacity(source.len());
    res.push_str(trim_end_ws(&source[..line_end(first_nl, 0)]));
    let mut sep = " ";
    let mut pos = first_nl + 1;
    while let Some(rel) = source[pos..].find('\n') {
        let nl = pos + rel;
        let lm = trim_both_ws(&source[pos..line_end(nl, pos)]);
        if lm.is_empty() {
            if sep == "\n" {
                res.push('\n');
            } else {
                sep = "\n";
            }
        } else {
            res.push_str(sep);
            res.push_str(lm);
            sep = " ";
        }
        pos = nl + 1;
    }
    res.push_str(sep);
    res.push_str(source[pos..].trim_start_matches([' ', '\t']));
    res
}

/// Fold a single newline into a space, multiple newlines to N - 1 newlines.
/// Presumes `source[offset] === '\n'`
fn fold_newline(b: &[u8], mut offset: usize) -> (String, usize) {
    let mut fold = String::new();
    let mut ch = b.get(offset + 1).copied();
    while matches!(ch, Some(b' ' | b'\t' | b'\n' | b'\r')) {
        if ch == Some(b'\r') && b.get(offset + 2) != Some(&b'\n') {
            break;
        }
        if ch == Some(b'\n') {
            fold.push('\n');
        }
        offset += 1;
        ch = b.get(offset + 1).copied();
    }
    if fold.is_empty() {
        fold.push(' ');
    }
    (fold, offset)
}

/// If `source[at..]` starts with `\uXXXX` or `\UXXXXXXXX` encoding a low
/// surrogate, returns it and the byte index just past it (minus nothing: the
/// index of the next unconsumed byte).
fn low_surrogate_escape(source: &str, at: usize) -> Option<(u32, usize)> {
    let rest = source.get(at..)?.as_bytes();
    if rest.first() != Some(&b'\\') {
        return None;
    }
    let len = match rest.get(1) {
        Some(b'u') => 4,
        Some(b'U') => 8,
        _ => return None,
    };
    let hex = rest.get(2..2 + len)?;
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
    if (0xdc00..0xe000).contains(&code) { Some((code, at + 2 + len)) } else { None }
}

// ---------------------------------------------------------------------------
// doc.toJS() (nodes/toJS.js, nodes/addPairToJSMap.js, nodes/Alias.js)
// ---------------------------------------------------------------------------

struct AnchorData {
    count: usize,
    alias_count: usize,
    /// `None` while the anchored node is still being converted
    res: Option<Value>,
}

/// Nesting depth of a node tree (iterative).
fn node_depth(root: &Node) -> usize {
    let mut max = 0;
    let mut work: Vec<(&Node, usize)> = vec![(root, 1)];
    while let Some((n, d)) = work.pop() {
        max = max.max(d);
        match &n.kind {
            NodeKind::Map { items, .. } => {
                for p in items {
                    work.extend(p.key.iter().chain(p.value.iter()).map(|c| (c, d + 1)));
                }
            }
            NodeKind::Seq { items, .. } => work.extend(items.iter().map(|c| (c, d + 1))),
            _ => {}
        }
    }
    max
}

/// `toJS` returns a self-referencing JS structure for `&a [*a]`, which a `Value` cannot hold.
const CIRCULAR_MSG: &str = "Circular alias reference (an alias refers to a node that contains it); not representable as a Value";

struct ToJs<'a> {
    /// set when an alias refers to a node that is still being converted
    circular: bool,
    /// alias node -> the last node with that anchor before it in document order
    targets: HashMap<*const Node, &'a Node>,
    targeted: HashSet<*const Node>,
    anchors: HashMap<*const Node, AnchorData>,
    max_alias_count: usize,
}

fn has_anchor(node: &Node) -> bool {
    !matches!(node.kind, NodeKind::Alias { .. }) && node.anchor.as_deref().is_some_and(|a| !a.is_empty())
}

impl<'a> ToJs<'a> {
    fn new(root: &'a Node) -> Self {
        let mut cx =
            ToJs { circular: false, targets: HashMap::new(), targeted: HashSet::new(), anchors: HashMap::new(), max_alias_count: 100 };
        let mut latest: HashMap<&'a str, &'a Node> = HashMap::new();
        cx.collect(root, &mut latest);
        cx
    }

    /// Visit order of `visit()`: a node, then its children (key before value).
    fn collect(&mut self, node: &'a Node, latest: &mut HashMap<&'a str, &'a Node>) {
        match &node.kind {
            NodeKind::Alias { source } => {
                if let Some(t) = latest.get(source.as_str()) {
                    self.targets.insert(node as *const Node, *t);
                    self.targeted.insert(*t as *const Node);
                }
            }
            _ => {
                if has_anchor(node)
                    && let Some(a) = node.anchor.as_deref()
                {
                    latest.insert(a, node);
                }
            }
        }
        match &node.kind {
            NodeKind::Map { items, .. } => {
                for p in items {
                    if let Some(k) = &p.key {
                        self.collect(k, latest);
                    }
                    if let Some(v) = &p.value {
                        self.collect(v, latest);
                    }
                }
            }
            NodeKind::Seq { items, .. } => {
                for it in items {
                    self.collect(it, latest);
                }
            }
            _ => {}
        }
    }

    /// `toJS(value, arg, ctx)` for a node
    fn convert(&mut self, node: &'a Node) -> Result<Value, String> {
        let ptr = node as *const Node;
        let anchored = has_anchor(node);
        if anchored {
            self.anchors.insert(ptr, AnchorData { count: 1, alias_count: 0, res: None });
        }
        let res = self.convert_node(node)?;
        if anchored
            && self.targeted.contains(&ptr)
            && let Some(d) = self.anchors.get_mut(&ptr)
        {
            d.res = Some(res.clone());
        }
        Ok(res)
    }

    fn convert_node(&mut self, node: &'a Node) -> Result<Value, String> {
        match &node.kind {
            NodeKind::Scalar { value, .. } => Ok(value.clone()),
            NodeKind::Seq { items, .. } => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.convert(it)?);
                }
                Ok(Value::Array(out))
            }
            NodeKind::Map { items, .. } => {
                let mut obj = ObjBuilder::default();
                for p in items {
                    let js_key = match &p.key {
                        Some(k) => self.convert(k)?,
                        None => Value::Null,
                    };
                    let string_key = match (&js_key, &p.key) {
                        (Value::Null, _) => String::new(),
                        (Value::Bool(b), _) => b.to_string(),
                        (Value::Number(n), _) => js_number_to_string(*n),
                        (Value::String(s), _) => s.clone(),
                        (_, Some(k)) => stringify_collection_key(k),
                        (_, None) => String::new(),
                    };
                    let js_value = match &p.value {
                        Some(v) => self.convert(v)?,
                        None => Value::Null,
                    };
                    obj.set(string_key, js_value);
                }
                Ok(Value::Object(obj.finish()))
            }
            NodeKind::Alias { source } => {
                let Some(found) = self.targets.get(&(node as *const Node)).copied() else {
                    return Err(format!("Unresolved alias (the anchor must be set before the alias): {source}"));
                };
                let fptr = found as *const Node;
                if !self.anchors.contains_key(&fptr) {
                    // Resolve anchors for Node.prototype.toJS()
                    self.convert(found)?;
                }
                let max = self.max_alias_count;
                if let Some(d) = self.anchors.get_mut(&fptr) {
                    d.count += 1;
                }
                let needs_count = self.anchors.get(&fptr).is_some_and(|d| d.alias_count == 0);
                if needs_count {
                    let c = self.get_alias_count(found);
                    if let Some(d) = self.anchors.get_mut(&fptr) {
                        d.alias_count = c;
                    }
                }
                if let Some(d) = self.anchors.get(&fptr)
                    && d.count * d.alias_count > max
                {
                    return Err("Excessive alias count indicates a resource exhaustion attack".to_string());
                }
                match self.anchors.get(&fptr).and_then(|d| d.res.clone()) {
                    Some(v) => Ok(v),
                    None => {
                        // JS hands out the object that is still being built; keep going so
                        // that any later error is reported like JS would
                        self.circular = true;
                        Ok(Value::Null)
                    }
                }
            }
        }
    }

    fn get_alias_count(&self, node: &Node) -> usize {
        match &node.kind {
            NodeKind::Alias { .. } => {
                let anchor = self.targets.get(&(node as *const Node)).and_then(|t| self.anchors.get(&(*t as *const Node)));
                anchor.map_or(0, |a| a.count * a.alias_count)
            }
            NodeKind::Map { items, .. } => {
                let mut count = 0;
                for p in items {
                    let kc = p.key.as_ref().map_or(1, |k| self.get_alias_count(k));
                    let vc = p.value.as_ref().map_or(1, |v| self.get_alias_count(v));
                    count = count.max(kc.max(vc));
                }
                count
            }
            NodeKind::Seq { items, .. } => {
                let mut count = 0;
                for it in items {
                    count = count.max(self.get_alias_count(it));
                }
                count
            }
            NodeKind::Scalar { .. } => 1,
        }
    }
}

/// A JS object under construction: `obj[key] = value` overwrites in place.
#[derive(Default)]
struct ObjBuilder {
    entries: Vec<(String, Value)>,
    index: HashMap<String, usize>,
}

impl ObjBuilder {
    fn set(&mut self, key: String, value: Value) {
        if self.entries.len() < 16 {
            if let Some(e) = self.entries.iter_mut().find(|(k, _)| *k == key) {
                e.1 = value;
                return;
            }
            self.entries.push((key, value));
            if self.entries.len() == 16 {
                for (i, (k, _)) in self.entries.iter().enumerate() {
                    self.index.insert(k.clone(), i);
                }
            }
            return;
        }
        match self.index.get(&key) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    /// Integer-like keys enumerate first in ascending order, then the other
    /// keys in insertion order (`OrdinaryOwnPropertyKeys`).
    fn finish(self) -> Vec<(String, Value)> {
        let mut entries = self.entries;
        if entries.iter().any(|(k, _)| array_index(k).is_some()) {
            let (mut ints, rest): (Vec<_>, Vec<_>) = entries.into_iter().partition(|(k, _)| array_index(k).is_some());
            ints.sort_by_key(|(k, _)| array_index(k));
            ints.extend(rest);
            entries = ints;
        }
        entries
    }
}

/// Canonical array index (`"0"`, `"17"`, up to 2^32 - 2)
fn array_index(k: &str) -> Option<u32> {
    let b = k.as_bytes();
    if b.is_empty() || b.len() > 10 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if b.len() > 1 && b[0] == b'0' {
        return None;
    }
    let n: u64 = k.parse().ok()?;
    if n < u64::from(u32::MAX) { Some(n as u32) } else { None }
}

// ---------------------------------------------------------------------------
// Stringifying collections used as map keys (stringify/*.js, flow style only)
//
// `toJS` turns a collection key into the text `key.toString(ctx)` in flow style
// (`[ a, b ]`, `{ a: 1 }`). Comments, scalar line folding and the original
// source text of null/bool/number scalars are not reproduced (a null scalar is
// written as it appeared when its range tells: empty, `~` or `null`).
// ---------------------------------------------------------------------------

const FLOW_INDENT_STEP: &str = "  ";
const LINE_WIDTH: usize = 80;

fn stringify_collection_key(key: &Node) -> String {
    match &key.kind {
        NodeKind::Alias { source } => format!("*{source}"),
        NodeKind::Scalar { .. } => fs_node(key, "", false, false),
        _ => fs_collection(key, "", false),
    }
}

/// `tagString(tag)` with the default `!!` handle
fn fs_tag(tag: &str) -> String {
    if let Some(rest) = tag.strip_prefix(DEFAULT_TAG_PREFIX) {
        let esc: String = rest
            .chars()
            .map(|c| match c {
                '!' => "%21".to_string(),
                ',' => "%2C".to_string(),
                '[' => "%5B".to_string(),
                ']' => "%5D".to_string(),
                '{' => "%7B".to_string(),
                '}' => "%7D".to_string(),
                c => c.to_string(),
            })
            .collect();
        format!("!!{esc}")
    } else if tag.starts_with('!') {
        tag.to_string()
    } else {
        format!("!<{tag}>")
    }
}

/// `stringify(item, ctx)` for a node in a flow collection
fn fs_node(n: &Node, indent: &str, implicit_key: bool, with_props: bool) -> String {
    let body = match &n.kind {
        NodeKind::Alias { source } => return format!("*{source}"),
        NodeKind::Scalar { value, .. } => fs_scalar(n, value, indent, implicit_key),
        _ => fs_collection(n, indent, false),
    };
    if !with_props {
        return body;
    }
    let mut props: Vec<String> = Vec::new();
    if let Some(a) = n.anchor.as_deref().filter(|a| !a.is_empty()) {
        props.push(format!("&{a}"));
    }
    if let Some(t) = n.tag.as_deref().filter(|t| !t.is_empty()) {
        props.push(fs_tag(t));
    }
    if props.is_empty() {
        return body;
    }
    let props = props.join(" ");
    if matches!(n.kind, NodeKind::Scalar { .. }) || body.starts_with(['{', '[']) {
        format!("{props} {body}")
    } else {
        format!("{props}\n{indent}{body}")
    }
}

fn fs_scalar(n: &Node, value: &Value, indent: &str, implicit_key: bool) -> String {
    match value {
        Value::Null => match n.range[1].saturating_sub(n.range[0]) {
            0 => String::new(),
            1 => "~".to_string(),
            _ => "null".to_string(),
        },
        Value::Bool(b) => b.to_string(),
        Value::Number(x) if x.is_nan() => ".nan".to_string(),
        Value::Number(x) if x.is_infinite() => if *x < 0.0 { "-.inf" } else { ".inf" }.to_string(),
        Value::Number(x) if *x == 0.0 && x.is_sign_negative() => "-0".to_string(),
        Value::Number(x) => js_number_to_string(*x),
        Value::String(s) => {
            let style = match &n.kind {
                NodeKind::Scalar { style, .. } => *style,
                _ => ScalarStyle::Plain,
            };
            fs_string(s, style, indent, implicit_key)
        }
        _ => String::new(),
    }
}

/// `stringifyString` in flow context (`inFlow` is always true)
fn fs_string(value: &str, style: ScalarStyle, indent: &str, implicit_key: bool) -> String {
    // force double quotes on control characters
    if style != ScalarStyle::QuoteDouble && value.chars().any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
    {
        return fs_double_quoted(value, implicit_key);
    }
    match style {
        ScalarStyle::QuoteDouble => return fs_double_quoted(value, implicit_key),
        ScalarStyle::QuoteSingle => return fs_single_quoted(value, indent, implicit_key),
        ScalarStyle::BlockLiteral | ScalarStyle::BlockFolded => {
            return fs_quoted(value, indent, implicit_key);
        }
        ScalarStyle::Plain => {}
    }
    let b = value.as_bytes();
    let first_bad = matches!(
        b.first(),
        Some(
            b'\n'
                | b'\t'
                | b' '
                | b','
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'#'
                | b'&'
                | b'*'
                | b'!'
                | b'|'
                | b'>'
                | b'\''
                | b'"'
                | b'%'
                | b'@'
                | b'`'
        )
    );
    let single_ind = value == "?" || value == "-";
    let ind_space = value.starts_with("? ") || value.starts_with("?\t") || value.starts_with("- ") || value.starts_with("-\t");
    let colon_ws = value.contains("\n ") || value.contains("\n\t") || value.contains(": ") || value.contains(":\t");
    let ws_nl = value.contains(" \n") || value.contains("\t\n");
    let hash = value.contains("\n#") || value.contains("\t#") || value.contains(" #");
    let end_bad = value.ends_with(['\n', '\t', ' ', ':']);
    if (implicit_key && value.contains('\n'))
        || value.contains(['[', ']', '{', '}', ','])
        || first_bad
        || single_ind
        || ind_space
        || colon_ws
        || ws_nl
        || hash
        || end_bad
    {
        return fs_quoted(value, indent, implicit_key);
    }
    // plain strings that would be read back as another type are quoted
    if [CoreTag::Null, CoreTag::Bool, CoreTag::IntOct, CoreTag::Int, CoreTag::IntHex, CoreTag::FloatNaN, CoreTag::FloatExp, CoreTag::Float]
        .iter()
        .any(|t| t.test(value))
    {
        return fs_quoted(value, indent, implicit_key);
    }
    value.to_string()
}

fn fs_quoted(value: &str, indent: &str, implicit_key: bool) -> String {
    let has_double = value.contains('"');
    let has_single = value.contains('\'');
    if has_double && !has_single { fs_single_quoted(value, indent, implicit_key) } else { fs_double_quoted(value, implicit_key) }
}

fn fs_single_quoted(value: &str, indent: &str, implicit_key: bool) -> String {
    if (implicit_key && value.contains('\n'))
        || value.contains(" \n")
        || value.contains("\t\n")
        || value.contains("\n ")
        || value.contains("\n\t")
    {
        return fs_double_quoted(value, implicit_key);
    }
    let mut out = String::from("'");
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => out.push_str("''"),
            '\n' => {
                out.push('\n');
                while chars.peek() == Some(&'\n') {
                    out.push('\n');
                    chars.next();
                }
                out.push('\n');
                out.push_str(indent);
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn fs_double_quoted(value: &str, _implicit_key: bool) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0}' => out.push_str("\\0"),
            '\u{7}' => out.push_str("\\a"),
            '\u{b}' => out.push_str("\\v"),
            '\u{1b}' => out.push_str("\\e"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `stringifyFlowCollection` (no comments) for a map or seq node whose own
/// context indent is `indent`.
fn fs_collection(n: &Node, indent: &str, all_null_inherited: bool) -> String {
    let (is_map, start, end) = match &n.kind {
        NodeKind::Map { .. } => (true, "{", "}"),
        _ => (false, "[", "]"),
    };
    // YAMLMap passes `itemIndent: indent`, YAMLSeq `indent + '  '`; the flow stringifier adds one step
    let item_indent = format!("{indent}{}{FLOW_INDENT_STEP}", if is_map { "" } else { FLOW_INDENT_STEP });
    let mut req_newline = false;
    let mut lines: Vec<String> = Vec::new();
    let mut lines_at_value = 0;
    let count = match &n.kind {
        NodeKind::Map { items, .. } => items.len(),
        NodeKind::Seq { items, .. } => items.len(),
        _ => 0,
    };
    let all_null = match &n.kind {
        NodeKind::Map { items, .. } => !all_null_inherited && items.iter().all(|p| p.value.is_none()),
        _ => false,
    } || all_null_inherited;
    for i in 0..count {
        let mut s = match &n.kind {
            NodeKind::Map { items, .. } => fs_pair(&items[i], &item_indent, all_null),
            NodeKind::Seq { items, .. } => fs_node(&items[i], &item_indent, false, true),
            _ => String::new(),
        };
        req_newline = req_newline || lines.len() > lines_at_value || s.contains('\n');
        if i + 1 < count {
            s.push(',');
        }
        lines.push(s);
        lines_at_value = lines.len();
    }
    if lines.is_empty() {
        return format!("{start}{end}");
    }
    if !req_newline {
        let len: usize = lines.iter().map(|l| utf16_len(l) + 2).sum::<usize>() + 2;
        req_newline = len > LINE_WIDTH;
    }
    if req_newline {
        let mut out = start.to_string();
        for line in &lines {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(&format!("\n{FLOW_INDENT_STEP}{indent}{line}"));
            }
        }
        format!("{out}\n{indent}{end}")
    } else {
        format!("{start} {} {end}", lines.join(" "))
    }
}

/// `stringifyPair` in flow context; `indent` is the context indent of the collection
fn fs_pair(p: &Pair, indent: &str, all_null: bool) -> String {
    let key = p.key.as_ref();
    let explicit_key = key.is_none_or(|k| matches!(k.kind, NodeKind::Map { .. } | NodeKind::Seq { .. }));
    let pair_indent = format!("{indent}{FLOW_INDENT_STEP}");
    let implicit = !explicit_key && !all_null;
    let key_str = match key {
        Some(k) => fs_node(k, &pair_indent, implicit, true),
        None => String::new(),
    };
    if all_null || p.value.is_none() {
        return if key_str.is_empty() {
            "?".to_string()
        } else if explicit_key {
            format!("? {key_str}")
        } else {
            key_str
        };
    }
    let mut s = if explicit_key { format!("? {key_str}\n{indent}:") } else { format!("{key_str}:") };
    let value = p.value.as_ref().expect("value");
    let value_str = fs_node(value, &pair_indent, false, true);
    let mut ws = " ".to_string();
    if !explicit_key && matches!(value.kind, NodeKind::Map { .. } | NodeKind::Seq { .. }) {
        let nl0 = value_str.find('\n');
        if let Some(nl0) = nl0 {
            let has_props_line = if value_str.starts_with(['&', '!']) {
                let mut sp0 = value_str.find(' ');
                if value_str.starts_with('&')
                    && let Some(sp) = sp0
                    && sp < nl0
                    && value_str.as_bytes().get(sp + 1) == Some(&b'!')
                {
                    sp0 = value_str[sp + 1..].find(' ').map(|x| x + sp + 1);
                }
                sp0.is_none_or(|sp| nl0 < sp)
            } else {
                false
            };
            if !has_props_line {
                ws = format!("\n{pair_indent}");
            }
        }
    } else if value_str.is_empty() || value_str.starts_with('\n') {
        ws = String::new();
    }
    s.push_str(&ws);
    s.push_str(&value_str);
    s
}

/// Verification helper (debug and test builds only): the CST (`new Parser().parse(src)`
/// of the JS library) as JSON text, with byte offsets.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn debug_cst(src: &str) -> String {
    let lexemes = Lexer::new(src).lex();
    let mut parser = Parser::new(src);
    parser.parse(&lexemes);
    let mut out = String::from("[");
    for (i, t) in parser.out.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        cst_json(t, src, &mut out);
    }
    out.push(']');
    dispose_tokens(parser.out);
    out
}

#[cfg(any(test, debug_assertions))]
fn cst_src_text(src: &str, s: &Src) -> String {
    match s.ty {
        Ty::DocMode => "\u{2}".to_string(),
        Ty::FlowErrorEnd => "\u{18}".to_string(),
        _ => src.get(s.offset..s.offset + s.len).unwrap_or("").to_string(),
    }
}

#[cfg(any(test, debug_assertions))]
fn cst_src(src: &str, s: &Src, out: &mut String, with_indent: bool) {
    out.push_str(&format!("{{\"type\":\"{}\",\"offset\":{}", s.ty.name(), s.offset));
    if with_indent {
        out.push_str(&format!(",\"indent\":{}", s.indent));
    }
    out.push_str(&format!(",\"source\":{}}}", json_quote(&cst_src_text(src, s))));
}

#[cfg(any(test, debug_assertions))]
fn cst_src_list(src: &str, list: &[Src], out: &mut String) {
    out.push('[');
    for (i, s) in list.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        cst_src(src, s, out, true);
    }
    out.push(']');
}

#[cfg(any(test, debug_assertions))]
fn cst_items(src: &str, items: &[Item], out: &mut String) {
    out.push('[');
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"start\":");
        cst_src_list(src, &it.start, out);
        if let Some(k) = &it.key {
            out.push_str(",\"key\":");
            cst_json(k, src, out);
        }
        if let Some(sep) = &it.sep {
            out.push_str(",\"sep\":");
            cst_src_list(src, sep, out);
        }
        if let Some(v) = &it.value {
            out.push_str(",\"value\":");
            cst_json(v, src, out);
        }
        if it.explicit_key {
            out.push_str(",\"explicitKey\":true");
        }
        out.push('}');
    }
    out.push(']');
}

#[cfg(any(test, debug_assertions))]
fn cst_json(t: &Token, src: &str, out: &mut String) {
    match t {
        Token::Src(s) => cst_src(src, s, out, true),
        Token::Directive(s) => cst_src(src, s, out, false),
        Token::Document(d) => {
            out.push_str(&format!("{{\"type\":\"document\",\"offset\":{},\"start\":", d.offset));
            cst_src_list(src, &d.start, out);
            if let Some(v) = &d.value {
                out.push_str(",\"value\":");
                cst_json(v, src, out);
            }
            if let Some(e) = &d.end {
                out.push_str(",\"end\":");
                cst_src_list(src, e, out);
            }
            out.push('}');
        }
        Token::DocEnd(d) => {
            out.push_str(&format!(
                "{{\"type\":\"doc-end\",\"offset\":{},\"source\":{}",
                d.offset,
                json_quote(src.get(d.offset..d.offset + d.len).unwrap_or(""))
            ));
            if let Some(e) = &d.end {
                out.push_str(",\"end\":");
                cst_src_list(src, e, out);
            }
            out.push('}');
        }
        Token::Error(e) => out.push_str(&format!(
            "{{\"type\":\"error\",\"offset\":{},\"message\":{},\"source\":{}}}",
            e.offset,
            json_quote(&e.message),
            json_quote(&e.source)
        )),
        Token::Flow(f) => {
            out.push_str(&format!(
                "{{\"type\":\"{}\",\"offset\":{},\"indent\":{},\"source\":{}",
                f.ty.name(),
                f.offset,
                f.indent,
                json_quote(src.get(f.offset..f.offset + f.len).unwrap_or(""))
            ));
            if let Some(e) = &f.end {
                out.push_str(",\"end\":");
                cst_src_list(src, e, out);
            }
            out.push('}');
        }
        Token::Block(b) => {
            out.push_str(&format!("{{\"type\":\"block-scalar\",\"offset\":{},\"indent\":{},\"props\":[", b.offset, b.indent));
            for (i, p) in b.props.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                cst_json(p, src, out);
            }
            out.push_str(&format!("],\"source\":{}}}", json_quote(src.get(b.src_off..b.src_off + b.src_len).unwrap_or(""))));
        }
        Token::Map(m) => {
            out.push_str(&format!("{{\"type\":\"block-map\",\"offset\":{},\"indent\":{},\"items\":", m.offset, m.indent));
            cst_items(src, &m.items, out);
            out.push('}');
        }
        Token::Seq(m) => {
            out.push_str(&format!("{{\"type\":\"block-seq\",\"offset\":{},\"indent\":{},\"items\":", m.offset, m.indent));
            cst_items(src, &m.items, out);
            out.push('}');
        }
        Token::FlowColl(f) => {
            out.push_str(&format!("{{\"type\":\"flow-collection\",\"offset\":{},\"indent\":{},\"start\":", f.offset, f.indent));
            cst_src(src, &f.start, out, true);
            out.push_str(",\"items\":");
            cst_items(src, &f.items, out);
            out.push_str(",\"end\":");
            cst_src_list(src, &f.end, out);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::String(v.to_string())
    }

    fn num(n: f64) -> Value {
        Value::Number(n)
    }

    fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn scalar_value(n: &Node) -> &Value {
        match &n.kind {
            NodeKind::Scalar { value, .. } => value,
            other => panic!("not a scalar: {other:?}"),
        }
    }

    fn map_items(n: &Node) -> &[Pair] {
        match &n.kind {
            NodeKind::Map { items, .. } => items,
            other => panic!("not a map: {other:?}"),
        }
    }

    fn seq_items(n: &Node) -> &[Node] {
        match &n.kind {
            NodeKind::Seq { items, .. } => items,
            other => panic!("not a seq: {other:?}"),
        }
    }

    fn key(n: &Node, i: usize) -> &Node {
        map_items(n)[i].key.as_ref().expect("key")
    }

    fn val(n: &Node, i: usize) -> &Node {
        map_items(n)[i].value.as_ref().expect("value")
    }

    fn root(doc: &Document) -> &Node {
        doc.contents.as_ref().expect("contents")
    }

    // ----- ranges -----

    #[test]
    fn ranges_of_block_map_and_flow_seq() {
        let doc = parse_document("a: 1\nb: [x, y]\n");
        assert!(doc.errors.is_empty() && doc.warnings.is_empty());
        let r = root(&doc);
        assert_eq!(r.range, [0, 15, 15]);
        assert_eq!(key(r, 0).range, [0, 1, 1]);
        assert_eq!(val(r, 0).range, [3, 4, 5]);
        let b = doc.get("b").expect("b");
        assert_eq!(b.range, [8, 14, 15]);
        assert!(matches!(b.kind, NodeKind::Seq { flow: true, .. }));
        assert_eq!(seq_items(b)[0].range, [9, 10, 10]);
        assert_eq!(seq_items(b)[1].range, [12, 13, 13]);
        assert_eq!(doc.range, [0, 15, 15]);
    }

    #[test]
    fn flow_seq_pair_is_a_single_pair_flow_map() {
        let doc = parse_document("[a: b, c]");
        let seq = root(&doc);
        assert_eq!(seq.range, [0, 9, 9]);
        let items = seq_items(seq);
        assert!(matches!(items[0].kind, NodeKind::Map { flow: true, .. }));
        assert_eq!(items[0].range, [1, 5, 5]);
        assert_eq!(key(&items[0], 0).range, [1, 2, 2]);
        assert_eq!(val(&items[0], 0).range, [4, 5, 5]);
        assert_eq!(items[1].range, [7, 8, 8]);
        assert_eq!(doc.to_js(), Ok(Value::Array(vec![obj(vec![("a", s("b"))]), s("c")])));
    }

    #[test]
    fn block_scalar_range_covers_header_and_body() {
        let doc = parse_document("run: |  # hdr\n  echo\nnext: 1\n");
        let r = root(&doc);
        let run = val(r, 0);
        assert_eq!(run.range, [5, 21, 21]);
        assert_eq!(run.comment.as_deref(), Some(" hdr"));
        match &run.kind {
            NodeKind::Scalar { value, style } => {
                assert_eq!(value, &s("echo\n"));
                assert_eq!(*style, ScalarStyle::BlockLiteral);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(key(r, 1).range, [21, 25, 25]);
    }

    #[test]
    fn ranges_are_byte_offsets_not_utf16() {
        // JS reports [3, 7, 12] for the value and [12, 14, 14] for the emoji key
        let doc = parse_document("é: \"日本\" # ü\n😀: x\n");
        assert!(doc.errors.is_empty());
        let r = root(&doc);
        assert_eq!(key(r, 0).range, [0, 2, 2]);
        assert_eq!(val(r, 0).range, [4, 12, 18]);
        assert_eq!(val(r, 0).comment.as_deref(), Some(" ü"));
        assert_eq!(key(r, 1).range, [18, 22, 22]);
        assert_eq!(val(r, 1).range, [24, 25, 26]);
    }

    #[test]
    fn bom_is_part_of_the_input() {
        let doc = parse_document("\u{feff}a: 1\n");
        assert!(doc.errors.is_empty());
        // JS: [1, 6, 6]; the BOM is 3 bytes
        assert_eq!(root(&doc).range, [3, 8, 8]);
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", num(1.0))])));
    }

    #[test]
    fn crlf_line_endings() {
        let doc = parse_document("a: 1\r\nb: |\r\n  x\r\n  y\r\nc: \"p\r\n  q\"\r\n");
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", num(1.0)), ("b", s("x\ny\n")), ("c", s("p q"))])));
    }

    // ----- comments -----

    #[test]
    fn trailing_comment_on_plain_scalar() {
        let doc = parse_document("run: cmd  # picky\nnext: 1\n");
        let r = root(&doc);
        let run = val(r, 0);
        assert_eq!(scalar_value(run), &s("cmd"));
        assert_eq!(run.range, [5, 8, 18]);
        assert_eq!(run.comment.as_deref(), Some(" picky"));
        assert_eq!(key(r, 1).range, [18, 22, 22]);
        assert!(key(r, 1).comment_before.is_none());
    }

    #[test]
    fn hash_without_preceding_space_is_not_a_comment() {
        let doc = parse_document("run: cmd#notcomment\n");
        assert!(doc.errors.is_empty());
        assert_eq!(doc.to_js(), Ok(obj(vec![("run", s("cmd#notcomment"))])));
        assert!(doc.get("run").expect("run").comment.is_none());
    }

    #[test]
    fn comments_before_items_and_after_the_document() {
        let doc = parse_document("# top\na: 1\n# between\nb: 2\n# end\n");
        let r = root(&doc);
        assert_eq!(r.range, [6, 26, 26]);
        assert_eq!(key(r, 0).comment_before.as_deref(), Some(" top"));
        assert_eq!(key(r, 1).comment_before.as_deref(), Some(" between"));
        assert_eq!(doc.comment.as_deref(), Some(" end"));
        assert_eq!(doc.range, [6, 26, 32]);
    }

    #[test]
    fn comments_in_a_block_seq() {
        let doc = parse_document("a:\n  - x # one\n  # two\n  - y\n");
        let seq = doc.get("a").expect("a");
        let items = seq_items(seq);
        assert_eq!(items[0].comment.as_deref(), Some(" one"));
        assert_eq!(items[0].range, [7, 8, 15]);
        assert_eq!(items[1].comment_before.as_deref(), Some(" two"));
        assert_eq!(items[1].range, [27, 28, 29]);
    }

    #[test]
    fn comments_after_quoted_scalars() {
        let doc = parse_document("- \"q\"  # c\n- 's' # d\n");
        let items = seq_items(root(&doc));
        assert_eq!(items[0].comment.as_deref(), Some(" c"));
        assert_eq!(items[0].range, [2, 5, 11]);
        assert_eq!(items[1].comment.as_deref(), Some(" d"));
        assert!(matches!(items[1].kind, NodeKind::Scalar { style: ScalarStyle::QuoteSingle, .. }));
    }

    #[test]
    fn document_comments_and_markers() {
        let doc = parse_document("--- # doc comment\na: 1\n...\n# after\n");
        assert_eq!(root(&doc).comment_before.as_deref(), Some(" doc comment"));
        assert_eq!(doc.comment.as_deref(), Some(" after"));
        assert_eq!(doc.range, [0, 23, 27]);
    }

    // ----- toJS -----

    #[test]
    fn to_js_scalar_types() {
        let doc =
            parse_document("a: 1\nb: -0\nc: 0x1F\nd: 0o17\ne: .5\nf: 1e3\ng: .inf\nh: -.inf\ni: .nan\nj: true\nk: ~\nl: \"1\"\nm: 1_000\n");
        let Value::Object(o) = doc.to_js().expect("toJS") else { panic!("object") };
        let get = |k: &str| &o.iter().find(|(n, _)| n == k).expect("key").1;
        assert_eq!(get("a"), &num(1.0));
        assert!(matches!(get("b"), Value::Number(n) if *n == 0.0 && n.is_sign_negative()));
        assert_eq!(get("c"), &num(31.0));
        assert_eq!(get("d"), &num(15.0));
        assert_eq!(get("e"), &num(0.5));
        assert_eq!(get("f"), &num(1000.0));
        assert_eq!(get("g"), &num(f64::INFINITY));
        assert_eq!(get("h"), &num(f64::NEG_INFINITY));
        assert!(matches!(get("i"), Value::Number(n) if n.is_nan()));
        assert_eq!(get("j"), &Value::Bool(true));
        assert_eq!(get("k"), &Value::Null);
        assert_eq!(get("l"), &s("1"));
        assert_eq!(get("m"), &s("1_000"));
    }

    #[test]
    fn to_js_big_numbers_match_js() {
        let doc = parse_document("a: 123456789012345678901234567890\nb: 0xFFFFFFFFFFFFFFFFFFFF\nc: 1e400\nd: 0.1\ne: 1e21\nf: 1e-7\n");
        assert_eq!(
            doc.to_js(),
            Ok(obj(vec![
                ("a", num(1.2345678901234568e29)),
                ("b", num(1.2089258196146292e24)),
                ("c", num(f64::INFINITY)),
                ("d", num(0.1)),
                ("e", num(1e21)),
                ("f", num(1e-7)),
            ]))
        );
        assert_eq!(js_number_to_string(1.2345678901234568e29), "1.2345678901234568e+29");
        assert_eq!(js_number_to_string(1e21), "1e+21");
        assert_eq!(js_number_to_string(1e-7), "1e-7");
        assert_eq!(js_number_to_string(123456.789), "123456.789");
        assert_eq!(js_number_to_string(0.000001), "0.000001");
        assert_eq!(js_number_to_string(-0.0), "0");
        assert_eq!(js_number_to_string(100.0), "100");
        assert_eq!(js_number_to_string(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn to_js_duplicate_key_keeps_first_position_and_last_value() {
        let doc = parse_document("a: 1\nb: 2\na: 3\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(doc.errors[0].code, "DUPLICATE_KEY");
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", num(3.0)), ("b", num(2.0))])));
    }

    #[test]
    fn to_js_orders_integer_keys_first_like_js_objects() {
        let doc = parse_document("10: a\n9: b\nz: c\na: d\n01: e\nz: f\n");
        assert_eq!(doc.to_js(), Ok(obj(vec![("1", s("e")), ("9", s("b")), ("10", s("a")), ("z", s("f")), ("a", s("d")),])));
    }

    #[test]
    fn to_js_stringifies_scalar_keys() {
        let doc = parse_document("1: a\n1.5: b\ntrue: c\nnull: d\n");
        assert_eq!(doc.to_js(), Ok(obj(vec![("1", s("a")), ("1.5", s("b")), ("true", s("c")), ("", s("d"))])));
    }

    #[test]
    fn to_js_resolves_aliases() {
        let doc = parse_document("a: &x [1, 2]\nb: *x\nc: &y 5\nd: *y\n");
        let one_two = Value::Array(vec![num(1.0), num(2.0)]);
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", one_two.clone()), ("b", one_two), ("c", num(5.0)), ("d", num(5.0)),])));
        let a = doc.get("a").expect("a");
        assert_eq!(a.anchor.as_deref(), Some("x"));
        let b = doc.get("b").expect("b");
        assert_eq!(b.kind, NodeKind::Alias { source: "x".to_string() });
        assert_eq!(b.range, [16, 18, 19]);
    }

    #[test]
    fn to_js_unresolved_alias_throws() {
        let doc = parse_document("a: *x\n");
        assert!(doc.errors.is_empty());
        assert_eq!(doc.to_js(), Err("Unresolved alias (the anchor must be set before the alias): x".to_string()));
        let e = parse("a: *x\n").expect_err("parse error");
        assert_eq!(e.message, "Unresolved alias (the anchor must be set before the alias): x");
    }

    #[test]
    fn to_js_alias_count_limit() {
        let mk = |n: usize| {
            let mut src = String::from("a: &a [x, y, z]\n");
            for i in 0..n {
                src.push_str(&format!("k{i}: *a\n"));
            }
            src
        };
        assert!(parse_document(&mk(99)).to_js().is_ok());
        assert_eq!(parse_document(&mk(100)).to_js(), Err("Excessive alias count indicates a resource exhaustion attack".to_string()));
    }

    #[test]
    fn to_js_circular_alias_is_an_error() {
        let doc = parse_document("&a [*a]\n");
        let e = doc.to_js().expect_err("circular");
        assert!(e.starts_with("Circular alias reference"), "{e}");
    }

    #[test]
    fn to_js_collection_keys_are_stringified() {
        let doc = parse_document("? [a, b]\n: 1\n");
        assert_eq!(doc.to_js(), Ok(obj(vec![("[ a, b ]", num(1.0))])));
    }

    #[test]
    fn to_js_empty_and_comment_only_documents() {
        assert_eq!(parse(""), Ok(Value::Null));
        assert_eq!(parse("# hi\n"), Ok(Value::Null));
        let doc = parse_document("");
        assert_eq!(doc.range, [0, 0, 0]);
        assert!(doc.errors.is_empty());
    }

    #[test]
    fn to_js_flow_and_nested() {
        assert_eq!(
            parse("{a: 1, b: [2, 3], c}"),
            Ok(obj(vec![("a", num(1.0)), ("b", Value::Array(vec![num(2.0), num(3.0)])), ("c", Value::Null),]))
        );
        assert_eq!(parse("- a: 1\n  b: 2\n- c\n"), Ok(Value::Array(vec![obj(vec![("a", num(1.0)), ("b", num(2.0))]), s("c")])));
        assert_eq!(
            parse("a: b\n  c\n\n  d\nq: \"x\n  y\"\nr: 'p\n\n q'\n"),
            Ok(obj(vec![("a", s("b c\nd")), ("q", s("x y")), ("r", s("p\nq"))]))
        );
        assert_eq!(parse("a: \"\\u00e9\\x41\\U0001F600 \\N\"\n"), Ok(obj(vec![("a", s("éA😀 \u{85}"))])));
    }

    #[test]
    fn merge_keys_are_plain_keys() {
        assert_eq!(
            parse("a: &a {x: 1}\nb:\n  <<: *a\n  y: 2\n"),
            Ok(obj(vec![("a", obj(vec![("x", num(1.0))])), ("b", obj(vec![("<<", obj(vec![("x", num(1.0))])), ("y", num(2.0))])),]))
        );
    }

    // ----- get / get_in -----

    #[test]
    fn get_and_get_in() {
        let doc = parse_document("jobs:\n  build:\n    steps:\n      - run: x\n      - run: y\n1: num\n");
        assert!(doc.errors.is_empty());
        assert!(matches!(doc.get("jobs").expect("jobs").kind, NodeKind::Map { .. }));
        let y = doc.get_in(&["jobs", "build", "steps", "1", "run"]).expect("y");
        assert_eq!(scalar_value(y), &s("y"));
        assert_eq!(y.range, [54, 55, 56]);
        let steps = doc.get_in(&["jobs", "build", "steps"]).expect("steps");
        assert_eq!(seq_items(steps).len(), 2);
        assert_eq!(steps.range, [32, 56, 56]);
        assert!(doc.get_in(&["jobs", "nope"]).is_none());
        assert!(doc.get_in(&["jobs", "build", "steps", "7"]).is_none());
        assert!(doc.get_in(&["jobs", "build", "steps", "x"]).is_none());
        assert!(doc.get_in(&["jobs", "build", "steps", "1", "run", "deeper"]).is_none());
        assert!(doc.get_in(&["nope", "build"]).is_none());
        // an empty path is the document contents
        assert!(std::ptr::eq(doc.get_in(&[]).expect("root"), root(&doc)));
        // numeric keys are numbers, not strings
        assert!(doc.get("1").is_none());
        // Node::get on a scalar
        assert!(y.get("run").is_none());
        // a missing value is undefined
        let doc = parse_document("a:\nb: 1\n");
        assert_eq!(scalar_value(doc.get("a").expect("a")), &Value::Null);
        let doc = parse_document("{a}");
        assert!(doc.get("a").is_none());
        assert!(parse_document("a").get("a").is_none());
        assert!(parse_document("").get_in(&["a"]).is_none());
    }

    // ----- errors -----

    #[test]
    fn duplicate_key_error() {
        let doc = parse_document("a: 1\na: 2\n");
        assert_eq!(doc.errors.len(), 1);
        let e = &doc.errors[0];
        assert_eq!(e.code, "DUPLICATE_KEY");
        assert_eq!(e.message, "Map keys must be unique at line 2, column 1:\n\na: 1\na: 2\n^\n");
        assert_eq!(e.pos, (5, 6));
        let err = parse("a: 1\na: 2\n").expect_err("first error");
        assert_eq!(&err, e);
    }

    #[test]
    fn bad_indent_error_shows_previous_line_context() {
        let doc = parse_document("a:\n  b: 1\n c: 2\n");
        assert_eq!(doc.errors.len(), 1);
        let e = &doc.errors[0];
        assert_eq!(e.code, "BAD_INDENT");
        assert_eq!(e.message, "All mapping items must start at the same column at line 3, column 1:\n\n  b: 1\n c: 2\n^\n");
        assert_eq!(e.pos, (10, 11));
        // error recovery keeps parsing
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", obj(vec![("b", num(1.0))])), ("c", num(2.0))])));
    }

    #[test]
    fn unterminated_flow_sequence() {
        let doc = parse_document("a: [1, 2\nb: 3\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(doc.errors[0].code, "BAD_INDENT");
        assert_eq!(
            doc.errors[0].message,
            "Flow sequence in block collection must be sufficiently indented and end with a ] at line 2, column 1:\n\na: [1, 2\nb: 3\n^\n"
        );
        assert_eq!(doc.errors[0].pos, (9, 10));
        let e = parse("a: [1, 2\nb: 3\n").expect_err("error");
        assert_eq!(e.code, "BAD_INDENT");
    }

    #[test]
    fn warnings_are_collected_but_ignored_by_parse() {
        let doc = parse_document("a: !foo bar\nb: !!int abc\n");
        assert!(doc.errors.is_empty());
        assert_eq!(doc.warnings.len(), 2);
        assert_eq!(doc.warnings[0].code, "TAG_RESOLVE_FAILED");
        assert_eq!(doc.warnings[0].message, "Unresolved tag: !foo at line 1, column 4:\n\na: !foo bar\n   ^^^^\n");
        assert_eq!(doc.warnings[0].pos, (3, 7));
        assert_eq!(doc.warnings[1].message, "Unresolved tag: tag:yaml.org,2002:int at line 2, column 4:\n\nb: !!int abc\n   ^^^^^\n");
        let a = doc.get("a").expect("a");
        assert_eq!(a.tag.as_deref(), Some("!foo"));
        assert_eq!(doc.get("b").expect("b").tag.as_deref(), Some("tag:yaml.org,2002:int"));
        assert_eq!(parse("a: !foo bar\n"), Ok(obj(vec![("a", s("bar"))])));
    }

    #[test]
    fn invalid_escape_and_compact_mapping() {
        let doc = parse_document("a: \"x\\q\"\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(doc.errors[0].code, "BAD_DQ_ESCAPE");
        assert_eq!(doc.errors[0].message, "Invalid escape sequence \\q at line 1, column 6:\n\na: \"x\\q\"\n     ^\n");
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", s("x\\q"))])));

        let doc = parse_document("a: b: c\n");
        assert_eq!(doc.errors[0].code, "BLOCK_AS_IMPLICIT_KEY");
        assert_eq!(doc.errors[0].message, "Nested mappings are not allowed in compact mappings at line 1, column 4:\n\na: b: c\n   ^\n");
    }

    #[test]
    fn tab_indentation_error() {
        let doc = parse_document("a:\n\tb: 1\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(doc.errors[0].code, "TAB_AS_INDENT");
        assert_eq!(doc.errors[0].message, "Tabs are not allowed as indentation at line 2, column 1:\n\na:\n\tb: 1\n^\n");
    }

    #[test]
    fn multiple_documents_error_is_added_to_the_first_document() {
        let doc = parse_document("a: 1\n---\nb: 2\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(doc.errors[0].code, "MULTIPLE_DOCS");
        assert_eq!(
            doc.errors[0].message,
            "Source contains multiple documents; please use YAML.parseAllDocuments() at line 2, column 1:\n\na: 1\n---\n^\n"
        );
        assert_eq!(doc.errors[0].pos, (5, 14));
        assert_eq!(doc.to_js(), Ok(obj(vec![("a", num(1.0))])));
    }

    #[test]
    fn error_columns_are_utf16_units() {
        let doc = parse_document("é: [1,\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(
            doc.errors[0].message,
            "Flow sequence in block collection must be sufficiently indented and end with a ] at line 2, column 1:\n\né: [1,\n\n^\n"
        );
        // JS pos [7, 8]: the source is 7 UTF-16 units long, 8 bytes here
        assert_eq!(doc.errors[0].pos, (8, 9));

        let doc = parse_document("😀: x\n  😀😀: [y\nz: 1\n");
        assert!(!doc.errors.is_empty());
        for e in &doc.errors {
            assert!(e.message.contains(" at line "), "{e:?}");
        }
    }

    #[test]
    fn long_lines_are_trimmed_around_the_error_column() {
        let src = format!("a: {}\n  bad: 1\n", "x".repeat(100));
        let doc = parse_document(&src);
        assert_eq!(doc.errors.len(), 2);
        assert_eq!(doc.errors[0].code, "BLOCK_AS_IMPLICIT_KEY");
        assert_eq!(
            doc.errors[0].message,
            format!("Nested mappings are not allowed in compact mappings at line 1, column 4:\n\na: {}…\n   ^\n", "x".repeat(76))
        );
        assert_eq!(doc.errors[1].code, "MULTILINE_IMPLICIT_KEY");
        assert_eq!(doc.errors[1].pos, (3, 109));
    }

    #[test]
    fn bad_directive_and_missing_doc_start() {
        let doc = parse_document("%YAML 1.3\n---\na: 1\n");
        assert_eq!(doc.errors.len(), 0);
        assert_eq!(doc.warnings.len(), 1);
        assert_eq!(doc.warnings[0].code, "BAD_DIRECTIVE");
        assert_eq!(doc.warnings[0].message, "Unsupported YAML version 1.3 at line 1, column 7:\n\n%YAML 1.3\n      ^^^\n");
        let doc = parse_document("%YAML 1.2\na: 1\n");
        assert_eq!(doc.errors[0].code, "MISSING_CHAR");
    }

    // ----- misc -----

    #[test]
    fn tags_anchors_and_collections() {
        let doc = parse_document("a: &x !!map {b: 1}\nc: !!seq [1]\nd: !!str 12\ne: ! 12\n");
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        let a = doc.get("a").expect("a");
        assert_eq!(a.anchor.as_deref(), Some("x"));
        assert_eq!(a.tag.as_deref(), Some("tag:yaml.org,2002:map"));
        assert_eq!(doc.get("c").expect("c").tag.as_deref(), Some("tag:yaml.org,2002:seq"));
        assert_eq!(scalar_value(doc.get("d").expect("d")), &s("12"));
        // the non-specific tag `!` forces a string
        assert_eq!(scalar_value(doc.get("e").expect("e")), &s("12"));
    }

    #[test]
    fn block_scalar_chomping_and_folding() {
        assert_eq!(
            parse("a: |\n  x\n  y\nb: >-\n  p\n  q\nc: |+\n  k\n\nd: >\n  m\n\n  n\n   more\n"),
            Ok(obj(vec![("a", s("x\ny\n")), ("b", s("p q")), ("c", s("k\n\n")), ("d", s("m\nn\n more\n")),]))
        );
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let n = 5000;
        let src = format!("{}{}", "[".repeat(n), "]".repeat(n));
        let doc = parse_document(&src);
        assert!(doc.errors.iter().any(|e| e.code == "RESOURCE_EXHAUSTION"));
        let src = "- ".repeat(n);
        let doc = parse_document(&src);
        assert!(doc.errors.iter().any(|e| e.code == "RESOURCE_EXHAUSTION"));
    }

    #[test]
    fn nesting_just_below_the_limit_works_on_a_small_stack() {
        let handle = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(|| {
                let n = MAX_DEPTH - 2;
                let src = format!("{}x{}", "[".repeat(n), "]".repeat(n));
                let doc = parse_document(&src);
                assert!(doc.errors.is_empty(), "{:?}", doc.errors.first());
                assert!(doc.to_js().is_ok());
            })
            .expect("spawn");
        handle.join().expect("no stack overflow");
    }

    #[test]
    fn large_inputs_are_handled_in_linear_time() {
        let mut src = String::new();
        for i in 0..20000 {
            src.push_str(&format!("key{i}: value {i}  # comment\n"));
        }
        let t = std::time::Instant::now();
        let doc = parse_document(&src);
        assert!(doc.errors.is_empty());
        let elapsed = t.elapsed();
        let Value::Object(o) = doc.to_js().expect("toJS") else { panic!() };
        assert_eq!(o.len(), 20000);
        assert!(elapsed.as_secs() < 5, "{elapsed:?}");
    }

    #[test]
    fn single_quoted_and_plain_multiline_scalars() {
        assert_eq!(parse("a: 'it''s'\nb: x\n  y\n"), Ok(obj(vec![("a", s("it's")), ("b", s("x y"))])));
    }
}
