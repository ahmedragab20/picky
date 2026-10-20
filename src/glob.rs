//! Glob matching: a port of picomatch 4.0.7 (`picomatch(globs, { dot })`), so patterns in
//! `.picky.yml`, Cargo workspaces and picky's own tables match exactly as they did in Node.
//!
//! picomatch turns a glob into JavaScript regex source; [`crate::jsre`] compiles that source.
//! Strings are handled as UTF-16 code units, as JavaScript does.

use crate::jsre;

type S = Vec<u16>;

fn s(x: &str) -> S {
    x.encode_utf16().collect()
}

/// `str.slice(start, end)` with JavaScript's negative-index rules (`slice(0, -0)` is empty).
fn slice(v: &[u16], start: i64, end: Option<i64>) -> S {
    let len = v.len() as i64;
    let clamp = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
    let (a, b) = (clamp(start), clamp(end.unwrap_or(len)));
    if a >= b { Vec::new() } else { v[a as usize..b as usize].to_vec() }
}

/// `str.slice(0, -n)`.
fn drop_last(v: &[u16], n: usize) -> S {
    slice(v, 0, Some(-(n as i64)))
}

fn cat(parts: &[&[u16]]) -> S {
    parts.concat()
}

fn contains(v: &[u16], c: char) -> bool {
    v.contains(&(c as u16))
}

fn starts_with(v: &[u16], p: &str) -> bool {
    v.starts_with(&s(p))
}

fn is_word(c: u16) -> bool {
    c < 0x80 && ((c as u8).is_ascii_alphanumeric() || c == b'_' as u16)
}

/// JavaScript's `String.prototype.trim` whitespace and line terminators.
fn is_js_space(c: u16) -> bool {
    matches!(c, 0x09..=0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

fn trim(v: &[u16]) -> S {
    let start = v.iter().position(|&c| !is_js_space(c)).unwrap_or(v.len());
    let end = v.iter().rposition(|&c| !is_js_space(c)).map_or(start, |e| e + 1);
    v[start..end.max(start)].to_vec()
}

fn is_line_terminator(c: u16) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

const REGEX_SPECIAL: &str = "-*+?.^${}(|)[]";

/// `utils.escapeRegex`
fn escape_regex(v: &[u16]) -> S {
    let mut out = Vec::with_capacity(v.len());
    for &c in v {
        if c < 0x80 && REGEX_SPECIAL.contains(c as u8 as char) {
            out.push(b'\\' as u16);
        }
        out.push(c);
    }
    out
}

fn has_regex_chars(v: &[u16]) -> bool {
    v.iter().any(|&c| c < 0x80 && REGEX_SPECIAL.contains(c as u8 as char))
}

/// `utils.toPosixSlashes`: backslashes not escaping a regex character become `/`.
fn to_posix_slashes(v: &[u16]) -> S {
    let mut out = v.to_vec();
    for i in 0..v.len() {
        if v[i] == b'\\' as u16 {
            let next = v.get(i + 1).copied();
            if !next.is_some_and(|c| c < 0x80 && "*+?^${}(|)[]".contains(c as u8 as char)) {
                out[i] = b'/' as u16;
            }
        }
    }
    out
}

/// `utils.escapeLast`
fn escape_last(input: &[u16], ch: char, last_idx: Option<i64>) -> S {
    let len = input.len() as i64;
    let from = last_idx.map_or(len - 1, |i| i.min(len - 1));
    let idx = if from < 0 && last_idx.is_some() {
        // lastIndexOf with a negative position only looks at index 0
        (input.first() == Some(&(ch as u16))).then_some(0)
    } else {
        (0..=from.max(-1)).rev().find(|&i| i >= 0 && input[i as usize] == ch as u16)
    };
    let Some(idx) = idx else { return input.to_vec() };
    if idx > 0 && input[idx as usize - 1] == b'\\' as u16 {
        return escape_last(input, ch, Some(idx - 1));
    }
    cat(&[&input[..idx as usize], &s("\\"), &input[idx as usize..]])
}

struct Chars {
    dot_literal: &'static str,
    plus_literal: &'static str,
    slash_literal: &'static str,
    one_char: &'static str,
    qmark: &'static str,
    dots_slash: &'static str,
    no_dot: &'static str,
    no_dots: &'static str,
    no_dot_slash: &'static str,
    no_dots_slash: &'static str,
    qmark_no_dot: &'static str,
    star: &'static str,
    start_anchor: &'static str,
}

const POSIX: Chars = Chars {
    dot_literal: "\\.",
    plus_literal: "\\+",
    slash_literal: "\\/",
    one_char: "(?=.)",
    qmark: "[^/]",
    dots_slash: "\\.{1,2}(?:\\/|$)",
    no_dot: "(?!\\.)",
    no_dots: "(?!(?:^|\\/)\\.{1,2}(?:\\/|$))",
    no_dot_slash: "(?!\\.{0,1}(?:\\/|$))",
    no_dots_slash: "(?!\\.{1,2}(?:\\/|$))",
    qmark_no_dot: "[^.\\/]",
    star: "[^/]*?",
    start_anchor: "(?:^|\\/)",
};

const WINDOWS: Chars = Chars {
    dot_literal: "\\.",
    plus_literal: "\\+",
    slash_literal: "[\\\\/]",
    one_char: "(?=.)",
    qmark: "[^\\\\/]",
    dots_slash: "\\.{1,2}(?:[\\\\/]|$)",
    no_dot: "(?!\\.)",
    no_dots: "(?!(?:^|[\\\\/])\\.{1,2}(?:[\\\\/]|$))",
    no_dot_slash: "(?!\\.{0,1}(?:[\\\\/]|$))",
    no_dots_slash: "(?!\\.{1,2}(?:[\\\\/]|$))",
    qmark_no_dot: "[^.\\\\/]",
    star: "[^\\\\/]*?",
    start_anchor: "(?:^|[\\\\/])",
};

const MAX_LENGTH: usize = 1024 * 64;

fn posix_class(name: &[u16]) -> Option<&'static str> {
    Some(match String::from_utf16(name).ok()?.as_str() {
        "alnum" => "a-zA-Z0-9",
        "alpha" => "a-zA-Z",
        "ascii" => "\\x00-\\x7F",
        "blank" => " \\t",
        "cntrl" => "\\x00-\\x1F\\x7F",
        "digit" => "0-9",
        "graph" => "\\x21-\\x7E",
        "lower" => "a-z",
        "print" => "\\x20-\\x7E ",
        "punct" => "\\-!\"#$%&'()\\*+,./:;<=>?@[\\]^_`{|}~",
        "space" => " \\t\\r\\n\\v\\f",
        "upper" => "A-Z",
        "word" => "A-Za-z0-9_",
        "xdigit" => "A-Fa-f0-9",
        _ => return None,
    })
}

fn replacement(input: &[u16]) -> Option<S> {
    match String::from_utf16(input).ok()?.as_str() {
        "***" => Some(s("*")),
        "**/**" | "**/**/**" => Some(s("**")),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    Bos,
    Text,
    Paren,
    Bracket,
    Brace,
    Comma,
    Slash,
    Dot,
    Dots,
    Qmark,
    Negate,
    Plus,
    At,
    Star,
    Globstar,
    MaybeSlash,
}

#[derive(Clone)]
struct Tok {
    ty: Ty,
    value: S,
    output: Option<S>,
    prev: usize,
    extglob: bool,
    star: bool,
    posix: bool,
    comma: bool,
    dots: bool,
    output_index: usize,
    tokens_index: usize,
}

impl Tok {
    fn new(ty: Ty, value: S, output: Option<S>) -> Self {
        Tok {
            ty,
            value,
            output,
            prev: 0,
            extglob: false,
            star: false,
            posix: false,
            comma: false,
            dots: false,
            output_index: 0,
            tokens_index: 0,
        }
    }
    /// `token.output != null ? token.output : token.value`
    fn out(&self) -> &[u16] {
        self.output.as_deref().unwrap_or(&self.value)
    }
    /// `token.output || token.value`
    fn out_or_value(&self) -> &[u16] {
        match &self.output {
            Some(o) if !o.is_empty() => o,
            _ => &self.value,
        }
    }
    /// `token.output` in a string concatenation (`undefined` prints as such).
    fn out_str(&self) -> S {
        self.output.clone().unwrap_or_else(|| s("undefined"))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ExtType {
    Negate,
    Qmark,
    Plus,
    Star,
}

struct Extglob {
    ty: ExtType,
    open: &'static str,
    close: S,
    inner: S,
    parens: i64,
    output: S,
    start_index: i64,
    tokens_index: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Counter {
    Parens,
    Brackets,
    Braces,
}

/// The result of `parse`: regex source and whether the glob was negated.
struct Parsed {
    output: S,
    negated: bool,
}

struct Opts {
    dot: bool,
    windows: bool,
    fastpaths: bool,
}

impl Opts {
    fn chars(&self) -> &'static Chars {
        if self.windows { &WINDOWS } else { &POSIX }
    }
}

// ---- repeated-extglob analysis (ReDoS guard) ----

fn split_top_level(input: &[u16]) -> Vec<S> {
    let mut parts = Vec::new();
    let (mut bracket, mut paren, mut quote) = (0i64, 0i64, 0);
    let mut value = Vec::new();
    let mut escaped = false;
    for &ch in input {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        if ch == b'\\' as u16 {
            value.push(ch);
            escaped = true;
            continue;
        }
        if ch == b'"' as u16 {
            quote = if quote == 1 { 0 } else { 1 };
            value.push(ch);
            continue;
        }
        if quote == 0 {
            if ch == b'[' as u16 {
                bracket += 1;
            } else if ch == b']' as u16 && bracket > 0 {
                bracket -= 1;
            } else if bracket == 0 {
                if ch == b'(' as u16 {
                    paren += 1;
                } else if ch == b')' as u16 && paren > 0 {
                    paren -= 1;
                } else if ch == b'|' as u16 && paren == 0 {
                    parts.push(std::mem::take(&mut value));
                    continue;
                }
            }
        }
        value.push(ch);
    }
    parts.push(value);
    parts
}

fn is_plain_branch(branch: &[u16]) -> bool {
    let mut escaped = false;
    for &ch in branch {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == b'\\' as u16 {
            escaped = true;
            continue;
        }
        if ch < 0x80 && "?*+@!()[]{}".contains(ch as u8 as char) {
            return false;
        }
    }
    true
}

fn normalize_simple_branch(branch: &[u16]) -> Option<S> {
    let mut value = trim(branch);
    // /^@\([^\\()[\]{}|]+\)$/
    loop {
        let ok = value.len() >= 4
            && starts_with(&value, "@(")
            && value.last() == Some(&(b')' as u16))
            && value[2..value.len() - 1].iter().all(|&c| !(c < 0x80 && "\\()[]{}|".contains(c as u8 as char)));
        if !ok {
            break;
        }
        value = value[2..value.len() - 1].to_vec();
    }
    if !is_plain_branch(&value) {
        return None;
    }
    // value.replace(/\\(.)/g, '$1')
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'\\' as u16 && i + 1 < value.len() && !is_line_terminator(value[i + 1]) {
            out.push(value[i + 1]);
            i += 2;
        } else {
            out.push(value[i]);
            i += 1;
        }
    }
    Some(out)
}

fn has_repeated_char_prefix_overlap(branches: &[S]) -> bool {
    let values: Vec<S> = branches.iter().filter_map(|b| normalize_simple_branch(b)).filter(|v| !v.is_empty()).collect();
    for i in 0..values.len() {
        for j in i + 1..values.len() {
            let (a, b) = (&values[i], &values[j]);
            let ch = a[0];
            if a.iter().any(|&c| c != ch) || b.iter().any(|&c| c != ch) {
                continue;
            }
            if a == b || a.starts_with(b) || b.starts_with(a) {
                return true;
            }
        }
    }
    false
}

struct Repeated {
    star: bool,
    body: S,
    end: usize,
}

fn parse_repeated_extglob(pattern: &[u16], require_end: bool) -> Option<Repeated> {
    let first = *pattern.first()?;
    if (first != b'+' as u16 && first != b'*' as u16) || pattern.get(1) != Some(&(b'(' as u16)) {
        return None;
    }
    let (mut bracket, mut paren, mut quote) = (0i64, 0i64, 0);
    let mut escaped = false;
    for i in 1..pattern.len() {
        let ch = pattern[i];
        if escaped {
            escaped = false;
            continue;
        }
        if ch == b'\\' as u16 {
            escaped = true;
            continue;
        }
        if ch == b'"' as u16 {
            quote = if quote == 1 { 0 } else { 1 };
            continue;
        }
        if quote == 1 {
            continue;
        }
        if ch == b'[' as u16 {
            bracket += 1;
            continue;
        }
        if ch == b']' as u16 && bracket > 0 {
            bracket -= 1;
            continue;
        }
        if bracket > 0 {
            continue;
        }
        if ch == b'(' as u16 {
            paren += 1;
            continue;
        }
        if ch == b')' as u16 {
            paren -= 1;
            if paren == 0 {
                if require_end && i != pattern.len() - 1 {
                    return None;
                }
                return Some(Repeated { star: first == b'*' as u16, body: pattern[2..i].to_vec(), end: i });
            }
        }
    }
    None
}

fn star_extglob_sequence_chars(pattern: &[u16]) -> Option<Vec<S>> {
    let mut index = 0;
    let mut chars = Vec::new();
    while index < pattern.len() {
        let m = parse_repeated_extglob(&pattern[index..], false)?;
        if !m.star {
            return None;
        }
        let branches: Vec<S> = split_top_level(&m.body).iter().map(|b| trim(b)).collect();
        if branches.len() != 1 {
            return None;
        }
        let branch = normalize_simple_branch(&branches[0])?;
        if branch.len() != 1 {
            return None;
        }
        chars.push(branch);
        index += m.end + 1;
    }
    (!chars.is_empty()).then_some(chars)
}

fn repeated_extglob_recursion(pattern: &[u16]) -> usize {
    let mut depth = 0;
    let mut value = trim(pattern);
    while let Some(m) = parse_repeated_extglob(&value, true) {
        depth += 1;
        value = trim(&m.body);
    }
    depth
}

/// (risky, safe output)
fn analyze_repeated_extglob(body: &[u16]) -> (bool, Option<S>) {
    let max = 0;
    let branches: Vec<S> = split_top_level(body).iter().map(|b| trim(b)).collect();
    if branches.len() > 1
        && (branches.iter().any(|b| b.is_empty())
            || branches.iter().any(|b| b.iter().all(|&c| c == b'*' as u16 || c == b'?' as u16))
            || has_repeated_char_prefix_overlap(&branches))
    {
        return (true, None);
    }
    let mut safe: Vec<S> = Vec::new();
    let mut saw_star_sequence = false;
    let mut combinable = true;
    for branch in &branches {
        if let Some(chars) = star_extglob_sequence_chars(branch) {
            saw_star_sequence = true;
            safe.extend(chars);
            continue;
        }
        if let Some(literal) = normalize_simple_branch(branch).filter(|l| l.len() == 1) {
            safe.push(literal);
            continue;
        }
        combinable = false;
        if repeated_extglob_recursion(branch) > max {
            return (true, None);
        }
    }
    if saw_star_sequence {
        if !combinable {
            return (true, None);
        }
        let mut unique: Vec<S> = Vec::new();
        for c in safe {
            if !unique.contains(&c) {
                unique.push(c);
            }
        }
        let mut source = if unique.len() == 1 {
            escape_regex(&unique[0])
        } else {
            let inner: Vec<S> = unique.iter().map(|c| escape_regex(c)).collect();
            cat(&[&s("["), &inner.concat(), &s("]")])
        };
        source.push(b'*' as u16);
        return (true, Some(source));
    }
    (false, None)
}

/// `expandRange` for `{a..b}`.
fn expand_range(mut args: Vec<S>) -> S {
    args.sort();
    let mut value = s("[");
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            value.push(b'-' as u16);
        }
        value.extend_from_slice(a);
    }
    value.push(b']' as u16);
    if jsre::is_valid(&value) {
        return value;
    }
    let escaped: Vec<S> = args.iter().map(|a| escape_regex(a)).collect();
    escaped.join(&s("..")[..])
}

// ---- parse ----

struct Parser<'o> {
    opts: &'o Opts,
    c: &'static Chars,
    input: S,
    len: usize,
    index: i64,
    start: i64,
    output: S,
    backtrack: bool,
    negated: bool,
    brackets: i64,
    braces: i64,
    parens: i64,
    quotes: i32,
    arena: Vec<Tok>,
    tokens: Vec<usize>,
    extglobs: Vec<Extglob>,
    braces_stack: Vec<usize>,
    stack: Vec<Counter>,
    prev: usize,
    star: S,
}

const BOS: usize = 0;

fn parse(input: &[u16], opts: &Opts) -> Result<Parsed, String> {
    let input = replacement(input).unwrap_or_else(|| input.to_vec());
    if input.len() > MAX_LENGTH {
        return Err(format!("Input length: {}, exceeds maximum allowed length: {MAX_LENGTH}", input.len()));
    }
    let c = opts.chars();
    // utils.removePrefix
    let input = if starts_with(&input, "./") { input[2..].to_vec() } else { input };
    let mut p = Parser {
        opts,
        c,
        len: input.len(),
        input,
        index: -1,
        start: 0,
        output: Vec::new(),
        backtrack: false,
        negated: false,
        brackets: 0,
        braces: 0,
        parens: 0,
        quotes: 0,
        arena: vec![Tok::new(Ty::Bos, Vec::new(), Some(Vec::new()))],
        tokens: vec![BOS],
        extglobs: Vec::new(),
        braces_stack: Vec::new(),
        stack: Vec::new(),
        prev: BOS,
        star: s(c.star),
    };
    if opts.fastpaths && !p.has_fastpath_blockers() {
        return Ok(p.fastpath());
    }
    p.run()?;
    Ok(Parsed { output: p.output, negated: p.negated })
}

impl Parser<'_> {
    fn globstar(&self) -> S {
        s(&format!("(?:(?:(?!{}{}).)*?)", self.c.start_anchor, if self.opts.dot { self.c.dots_slash } else { self.c.dot_literal }))
    }

    fn nodot(&self) -> &'static str {
        if self.opts.dot { "" } else { self.c.no_dot }
    }

    fn eos(&self) -> bool {
        self.index == self.len as i64 - 1
    }

    fn peek(&self, n: i64) -> Option<u16> {
        let i = self.index + n;
        if i < 0 { None } else { self.input.get(i as usize).copied() }
    }

    fn peek_is(&self, n: i64, ch: char) -> bool {
        self.peek(n) == Some(ch as u16)
    }

    fn advance(&mut self) -> S {
        self.index += 1;
        self.peek(0).map(|c| vec![c]).unwrap_or_default()
    }

    fn remaining(&self) -> S {
        slice(&self.input, self.index + 1, None)
    }

    fn increment(&mut self, t: Counter) {
        *self.counter(t) += 1;
        self.stack.push(t);
    }

    fn decrement(&mut self, t: Counter) {
        *self.counter(t) -= 1;
        self.stack.pop();
    }

    fn counter(&mut self, t: Counter) -> &mut i64 {
        match t {
            Counter::Parens => &mut self.parens,
            Counter::Brackets => &mut self.brackets,
            Counter::Braces => &mut self.braces,
        }
    }

    fn append_value(&mut self, value: &[u16]) {
        self.output.extend_from_slice(value);
    }

    fn push(&mut self, mut tok: Tok) {
        let prev = self.prev;
        if self.arena[prev].ty == Ty::Globstar {
            let is_brace = self.braces > 0 && (tok.ty == Ty::Comma || tok.ty == Ty::Brace);
            let is_extglob = tok.extglob || (!self.extglobs.is_empty() && tok.ty == Ty::Paren);
            if tok.ty != Ty::Slash && tok.ty != Ty::Paren && !is_brace && !is_extglob {
                let n = self.arena[prev].output.as_ref().map_or(0, |o| o.len());
                self.output = drop_last(&self.output, n);
                let star = self.star.clone();
                let pt = &mut self.arena[prev];
                pt.ty = Ty::Star;
                pt.value = s("*");
                pt.output = Some(star.clone());
                self.output.extend_from_slice(&star);
            }
        }
        if let Some(e) = self.extglobs.last_mut()
            && tok.ty != Ty::Paren
        {
            e.inner.extend_from_slice(&tok.value);
        }
        if !tok.value.is_empty() || tok.output.as_ref().is_some_and(|o| !o.is_empty()) {
            let out = tok.out().to_vec();
            self.append_value(&out);
        }
        if self.arena[prev].ty == Ty::Text && tok.ty == Ty::Text {
            let pt = &mut self.arena[prev];
            let mut merged = pt.out_or_value().to_vec();
            merged.extend_from_slice(&tok.value);
            pt.output = Some(merged);
            pt.value.extend_from_slice(&tok.value);
            return;
        }
        tok.prev = prev;
        self.arena.push(tok);
        let id = self.arena.len() - 1;
        self.tokens.push(id);
        self.prev = id;
    }

    fn extglob_open(&mut self, ty: ExtType, ty_tok: Ty, value: S) {
        let close = match ty {
            ExtType::Negate => s(&format!(")){})", self.c.star)),
            ExtType::Qmark => s(")?"),
            ExtType::Plus => s(")+"),
            ExtType::Star => s(")*"),
        };
        let open = if ty == ExtType::Negate { "(?:(?!(?:" } else { "(?:" };
        let token = Extglob {
            ty,
            open,
            close,
            inner: Vec::new(),
            parens: self.parens,
            output: self.output.clone(),
            start_index: self.index,
            tokens_index: self.tokens.len(),
        };
        let output = s(token.open);
        self.increment(Counter::Parens);
        let first = if self.output.is_empty() { s(self.c.one_char) } else { Vec::new() };
        self.push(Tok::new(ty_tok, value, Some(first)));
        let paren_value = self.advance();
        let mut paren = Tok::new(Ty::Paren, paren_value, Some(output));
        paren.extglob = true;
        self.push(paren);
        self.extglobs.push(token);
    }

    fn extglob_close(&mut self, mut token: Extglob, value: S) -> Result<(), String> {
        let literal = slice(&self.input, token.start_index, Some(self.index + 1));
        let body = slice(&self.input, token.start_index + 2, Some(self.index));
        let (risky, safe_output) = analyze_repeated_extglob(&body);

        if (token.ty == ExtType::Plus || token.ty == ExtType::Star) && risky {
            let safe = safe_output.map(|so| {
                let prefix = if token.output.is_empty() { s(self.c.one_char) } else { Vec::new() };
                cat(&[&prefix, &so])
            });
            let open_id = self.tokens[token.tokens_index];
            let open_output = match safe {
                Some(so) if !so.is_empty() => so,
                _ => escape_regex(&literal),
            };
            {
                let open = &mut self.arena[open_id];
                open.ty = Ty::Text;
                open.value = literal;
                open.output = Some(open_output.clone());
            }
            for i in token.tokens_index + 1..self.tokens.len() {
                let id = self.tokens[i];
                self.arena[id].value = Vec::new();
                self.arena[id].output = Some(Vec::new());
            }
            self.output = cat(&[&token.output, &open_output]);
            self.backtrack = true;
            let mut paren = Tok::new(Ty::Paren, value, Some(Vec::new()));
            paren.extglob = true;
            self.push(paren);
            self.decrement(Counter::Parens);
            return Ok(());
        }

        let mut output = token.close.clone();
        if token.ty == ExtType::Negate {
            let mut extglob_star = self.star.clone();
            if token.inner.len() > 1 && contains(&token.inner, '/') {
                extglob_star = self.globstar();
            }
            let rest = self.remaining();
            let all_parens = !rest.is_empty() && rest.iter().all(|&c| c == b')' as u16);
            if extglob_star != self.star || self.eos() || all_parens {
                token.close = cat(&[&s(")$))"), &extglob_star]);
                output = token.close.clone();
            }
            // /^\.[^\\/.]+$/
            let dotted_ext = rest.len() > 1
                && rest[0] == b'.' as u16
                && rest[1..].iter().all(|&c| c != b'\\' as u16 && c != b'/' as u16 && c != b'.' as u16);
            if contains(&token.inner, '*') && dotted_ext {
                let nested = Opts { dot: self.opts.dot, windows: self.opts.windows, fastpaths: false };
                let expression = parse(&rest, &nested)?.output;
                token.close = cat(&[&s(")"), &expression, &s(")"), &extglob_star, &s(")")]);
                output = token.close.clone();
            }
        }
        let mut paren = Tok::new(Ty::Paren, value, Some(output));
        paren.extglob = true;
        self.push(paren);
        self.decrement(Counter::Parens);
        Ok(())
    }

    fn has_fastpath_blockers(&self) -> bool {
        // /(^[*!]|[/()[\]{}"])/
        matches!(self.input.first(), Some(&c) if c == b'*' as u16 || c == b'!' as u16)
            || self.input.iter().any(|&c| c < 0x80 && "/()[]{}\"".contains(c as u8 as char))
    }

    /// The fast path inside `parse` for globs without structure.
    fn fastpath(&self) -> Parsed {
        let input = &self.input;
        let qmark_no_dot = if self.opts.dot { self.c.qmark } else { self.c.qmark_no_dot };
        let qmark = s(self.c.qmark);
        let mut backslashes = false;
        let mut output: S = Vec::new();
        // input.replace(/(\\?)((\W)(\3*))/g, ...)
        let mut i = 0;
        while i < input.len() {
            let c = input[i];
            let (esc, first_at) = if c == b'\\' as u16 && input.get(i + 1).is_some_and(|&n| !is_word(n)) {
                (true, i + 1)
            } else if !is_word(c) {
                (false, i)
            } else {
                output.push(c);
                i += 1;
                continue;
            };
            let first = input[first_at];
            let mut end = first_at + 1;
            while end < input.len() && input[end] == first {
                end += 1;
            }
            let m = &input[i..end];
            let rest_len = end - first_at - 1;
            let chars_len = end - first_at;
            let esc_s: S = if esc { s("\\") } else { Vec::new() };
            let repl: S = if first == b'\\' as u16 {
                backslashes = true;
                m.to_vec()
            } else if first == b'?' as u16 {
                if esc {
                    cat(&[&esc_s, &[first], &qmark.repeat(rest_len)])
                } else if i == 0 {
                    cat(&[&s(qmark_no_dot), &qmark.repeat(rest_len)])
                } else {
                    qmark.repeat(chars_len)
                }
            } else if first == b'.' as u16 {
                s(self.c.dot_literal).repeat(chars_len)
            } else if first == b'*' as u16 {
                if esc { cat(&[&esc_s, &[first], if rest_len > 0 { &self.star } else { &[] }]) } else { self.star.clone() }
            } else if esc {
                m.to_vec()
            } else {
                cat(&[&s("\\"), m])
            };
            output.extend_from_slice(&repl);
            i = end;
        }
        if backslashes {
            // output.replace(/\\+/g, m => m.length % 2 === 0 ? '\\\\' : '\\')
            let mut out = Vec::with_capacity(output.len());
            let mut j = 0;
            while j < output.len() {
                if output[j] == b'\\' as u16 {
                    let mut k = j;
                    while k < output.len() && output[k] == b'\\' as u16 {
                        k += 1;
                    }
                    out.extend_from_slice(&if (k - j) % 2 == 0 { s("\\\\") } else { s("\\") });
                    j = k;
                } else {
                    out.push(output[j]);
                    j += 1;
                }
            }
            output = out;
        }
        Parsed { output: cat(&[&s("^(?:"), &output, &s(")$")]), negated: false }
    }

    fn run(&mut self) -> Result<(), String> {
        while !self.eos() {
            let mut value = self.advance();
            if value == [0] {
                continue;
            }
            let ch = value[0];

            if ch == b'\\' as u16 {
                let next = self.peek(1);
                if next == Some(b'/' as u16) {
                    continue;
                }
                if next == Some(b'.' as u16) || next == Some(b';' as u16) {
                    continue;
                }
                if next.is_none() {
                    value.push(b'\\' as u16);
                    self.push(Tok::new(Ty::Text, value, None));
                    continue;
                }
                let rem = self.remaining();
                let slashes = rem.iter().take_while(|&&c| c == b'\\' as u16).count();
                if slashes > 2 {
                    self.index += slashes as i64;
                    if slashes % 2 != 0 {
                        value.push(b'\\' as u16);
                    }
                }
                let next = self.advance();
                value.extend_from_slice(&next);
                if self.brackets == 0 {
                    self.push(Tok::new(Ty::Text, value, None));
                    continue;
                }
            }

            let prev = self.prev;
            let prev_value = self.arena[prev].value.clone();
            if self.brackets > 0 && (value != s("]") || prev_value == s("[") || prev_value == s("[^")) {
                if value == s(":") {
                    let inner = slice(&prev_value, 1, None);
                    if contains(&inner, '[') {
                        self.arena[prev].posix = true;
                        if contains(&inner, ':') {
                            let idx = prev_value.iter().rposition(|&c| c == b'[' as u16).unwrap();
                            let pre = &prev_value[..idx];
                            let rest = slice(&prev_value, idx as i64 + 2, None);
                            if let Some(posix) = posix_class(&rest) {
                                self.arena[prev].value = cat(&[pre, &s(posix)]);
                                self.backtrack = true;
                                self.advance();
                                let bos_empty = self.arena[BOS].output.as_ref().is_none_or(|o| o.is_empty());
                                if bos_empty && self.tokens.iter().position(|&t| t == prev) == Some(1) {
                                    self.arena[BOS].output = Some(s(self.c.one_char));
                                }
                                continue;
                            }
                        }
                    }
                }
                if (value == s("[") && !self.peek_is(1, ':')) || (value == s("-") && self.peek_is(1, ']')) {
                    value = cat(&[&s("\\"), &value]);
                }
                if value == s("]") && (prev_value == s("[") || prev_value == s("[^")) {
                    value = cat(&[&s("\\"), &value]);
                }
                self.arena[prev].value.extend_from_slice(&value);
                self.append_value(&value);
                continue;
            }

            if self.quotes == 1 && value != s("\"") {
                let value = escape_regex(&value);
                self.arena[prev].value.extend_from_slice(&value);
                self.append_value(&value);
                continue;
            }

            if value == s("\"") {
                self.quotes = if self.quotes == 1 { 0 } else { 1 };
                continue;
            }

            if value == s("(") {
                self.increment(Counter::Parens);
                self.push(Tok::new(Ty::Paren, value, None));
                continue;
            }

            if value == s(")") {
                let parens = self.parens;
                if let Some(e) = self.extglobs.pop_if(|e| parens == e.parens + 1) {
                    self.extglob_close(e, value)?;
                    continue;
                }
                let out = if self.parens != 0 { s(")") } else { s("\\)") };
                self.push(Tok::new(Ty::Paren, value, Some(out)));
                self.decrement(Counter::Parens);
                continue;
            }

            if value == s("[") {
                if !contains(&self.remaining(), ']') {
                    value = s("\\[");
                } else {
                    self.increment(Counter::Brackets);
                }
                self.push(Tok::new(Ty::Bracket, value, None));
                continue;
            }

            if value == s("]") {
                if self.arena[prev].ty == Ty::Bracket && self.arena[prev].value.len() == 1 {
                    self.push(Tok::new(Ty::Text, value, Some(s("\\]"))));
                    continue;
                }
                if self.brackets == 0 {
                    self.push(Tok::new(Ty::Text, value, Some(s("\\]"))));
                    continue;
                }
                self.decrement(Counter::Brackets);
                let prev_value = slice(&self.arena[prev].value, 1, None);
                if !self.arena[prev].posix && prev_value.first() == Some(&(b'^' as u16)) && !contains(&prev_value, '/') {
                    value = s("/]");
                }
                self.arena[prev].value.extend_from_slice(&value);
                self.append_value(&value);
                if has_regex_chars(&prev_value) {
                    continue;
                }
                let full = self.arena[prev].value.clone();
                let escaped = escape_regex(&full);
                self.output = drop_last(&self.output, full.len());
                let wrapped = cat(&[&s("(?:"), &escaped, &s("|"), &full, &s(")")]);
                self.output.extend_from_slice(&wrapped);
                self.arena[prev].value = wrapped;
                continue;
            }

            if value == s("{") {
                self.increment(Counter::Braces);
                let mut open = Tok::new(Ty::Brace, value, Some(s("(")));
                open.output_index = self.output.len();
                open.tokens_index = self.tokens.len();
                self.push(open);
                // push never merges a brace, so the new token is the brace
                self.braces_stack.push(self.prev);
                continue;
            }

            if value == s("}") {
                let Some(&brace) = self.braces_stack.last() else {
                    self.push(Tok::new(Ty::Text, value.clone(), Some(value)));
                    continue;
                };
                let mut output = s(")");
                if self.arena[brace].dots {
                    let arr = self.tokens.clone();
                    let mut range: Vec<S> = Vec::new();
                    for i in (0..arr.len()).rev() {
                        self.tokens.pop();
                        if self.arena[arr[i]].ty == Ty::Brace {
                            break;
                        }
                        if self.arena[arr[i]].ty != Ty::Dots {
                            range.insert(0, self.arena[arr[i]].value.clone());
                        }
                    }
                    output = expand_range(range);
                    self.backtrack = true;
                }
                if !self.arena[brace].comma && !self.arena[brace].dots {
                    let out = slice(&self.output, 0, Some(self.arena[brace].output_index as i64));
                    let from = self.arena[brace].tokens_index.min(self.tokens.len());
                    let toks: Vec<usize> = self.tokens[from..].to_vec();
                    self.arena[brace].value = s("\\{");
                    self.arena[brace].output = Some(s("\\{"));
                    value = s("\\}");
                    output = s("\\}");
                    self.output = out;
                    for t in toks {
                        let o = self.arena[t].out_or_value().to_vec();
                        self.output.extend_from_slice(&o);
                    }
                }
                self.push(Tok::new(Ty::Brace, value, Some(output)));
                self.decrement(Counter::Braces);
                self.braces_stack.pop();
                continue;
            }

            if value == s("|") {
                if let Some(e) = self.extglobs.last_mut() {
                    let _ = e; // conditions are counted but never read
                }
                self.push(Tok::new(Ty::Text, value, None));
                continue;
            }

            if value == s(",") {
                let mut output = value.clone();
                if let Some(&brace) = self.braces_stack.last()
                    && self.stack.last() == Some(&Counter::Braces)
                {
                    self.arena[brace].comma = true;
                    output = s("|");
                }
                self.push(Tok::new(Ty::Comma, value, Some(output)));
                continue;
            }

            if value == s("/") {
                if self.arena[prev].ty == Ty::Dot && self.index == self.start + 1 {
                    self.start = self.index + 1;
                    self.output = Vec::new();
                    self.tokens.pop();
                    self.prev = BOS;
                    continue;
                }
                self.push(Tok::new(Ty::Slash, value, Some(s(self.c.slash_literal))));
                continue;
            }

            if value == s(".") {
                if self.braces > 0 && self.arena[prev].ty == Ty::Dot {
                    if self.arena[prev].value == s(".") {
                        self.arena[prev].output = Some(s(self.c.dot_literal));
                    }
                    let brace = *self.braces_stack.last().expect("open brace");
                    let pt = &mut self.arena[prev];
                    pt.ty = Ty::Dots;
                    let mut o = pt.out_str();
                    o.extend_from_slice(&value);
                    pt.output = Some(o);
                    pt.value.extend_from_slice(&value);
                    self.arena[brace].dots = true;
                    continue;
                }
                let pty = self.arena[prev].ty;
                if self.braces + self.parens == 0 && pty != Ty::Bos && pty != Ty::Slash {
                    self.push(Tok::new(Ty::Text, value, Some(s(self.c.dot_literal))));
                    continue;
                }
                self.push(Tok::new(Ty::Dot, value, Some(s(self.c.dot_literal))));
                continue;
            }

            if value == s("?") {
                let is_group = prev_value == s("(");
                if !is_group && self.peek_is(1, '(') && !self.peek_is(2, '?') {
                    self.extglob_open(ExtType::Qmark, Ty::Qmark, value);
                    continue;
                }
                if self.arena[prev].ty == Ty::Paren {
                    let next = self.peek(1);
                    let special = |c: Option<u16>| c.is_some_and(|c| c < 0x80 && "!=<:".contains(c as u8 as char));
                    let mut output = value.clone();
                    if (prev_value == s("(") && !special(next)) || (next == Some(b'<' as u16) && !lookbehind_or_name(&self.remaining())) {
                        output = s("\\?");
                    }
                    self.push(Tok::new(Ty::Text, value, Some(output)));
                    continue;
                }
                let pty = self.arena[prev].ty;
                if !self.opts.dot && (pty == Ty::Slash || pty == Ty::Bos) {
                    self.push(Tok::new(Ty::Qmark, value, Some(s(self.c.qmark_no_dot))));
                    continue;
                }
                self.push(Tok::new(Ty::Qmark, value, Some(s(self.c.qmark))));
                continue;
            }

            if value == s("!") {
                if self.peek_is(1, '(') {
                    let special = self.peek(3).is_some_and(|c| c < 0x80 && "!=<:".contains(c as u8 as char));
                    if !self.peek_is(2, '?') || !special {
                        self.extglob_open(ExtType::Negate, Ty::Negate, value);
                        continue;
                    }
                }
                if self.index == 0 {
                    self.negate();
                    continue;
                }
            }

            if value == s("+") {
                if self.peek_is(1, '(') && !self.peek_is(2, '?') {
                    self.extglob_open(ExtType::Plus, Ty::Plus, value);
                    continue;
                }
                if prev_value == s("(") {
                    self.push(Tok::new(Ty::Plus, value, Some(s(self.c.plus_literal))));
                    continue;
                }
                let pty = self.arena[prev].ty;
                if pty == Ty::Bracket || pty == Ty::Paren || pty == Ty::Brace || self.parens > 0 {
                    self.push(Tok::new(Ty::Plus, value, None));
                    continue;
                }
                self.push(Tok::new(Ty::Plus, s(self.c.plus_literal), None));
                continue;
            }

            if value == s("@") {
                if self.peek_is(1, '(') && !self.peek_is(2, '?') {
                    let mut at = Tok::new(Ty::At, value, Some(Vec::new()));
                    at.extglob = true;
                    self.push(at);
                    continue;
                }
                self.push(Tok::new(Ty::Text, value, None));
                continue;
            }

            if value != s("*") {
                if value == s("$") || value == s("^") {
                    value = cat(&[&s("\\"), &value]);
                }
                // /^[^@![\].,$*+?^{}()|\\/]+/
                let rem = self.remaining();
                let n = rem.iter().take_while(|&&c| !(c < 0x80 && "@![].,$*+?^{}()|\\/".contains(c as u8 as char))).count();
                if n > 0 {
                    value.extend_from_slice(&rem[..n]);
                    self.index += n as i64;
                }
                self.push(Tok::new(Ty::Text, value, None));
                continue;
            }

            // stars
            if self.arena[prev].ty == Ty::Globstar || self.arena[prev].star {
                let star = self.star.clone();
                let pt = &mut self.arena[prev];
                pt.ty = Ty::Star;
                pt.star = true;
                pt.value.extend_from_slice(&value);
                pt.output = Some(star);
                self.backtrack = true;
                continue;
            }

            let mut rest = self.remaining();
            if rest.len() >= 2 && rest[0] == b'(' as u16 && rest[1] != b'?' as u16 {
                self.extglob_open(ExtType::Star, Ty::Star, value);
                continue;
            }

            if self.arena[prev].ty == Ty::Star {
                let prior = self.arena[prev].prev;
                let before = self.arena[prior].prev;
                let prior_ty = self.arena[prior].ty;
                let is_start = prior_ty == Ty::Slash || prior_ty == Ty::Bos;
                // bos has no prev: `before` is only meaningful when prior isn't bos
                let after_star = prior != BOS && matches!(self.arena[before].ty, Ty::Star | Ty::Globstar);
                let is_brace = self.braces > 0 && (prior_ty == Ty::Comma || prior_ty == Ty::Brace);
                let is_extglob = !self.extglobs.is_empty() && prior_ty == Ty::Paren;
                if !is_start && prior_ty != Ty::Paren && !is_brace && !is_extglob {
                    self.push(Tok::new(Ty::Star, value, Some(Vec::new())));
                    continue;
                }

                // strip consecutive `/**/`
                while starts_with(&rest, "/**") {
                    let after = self.input.get((self.index + 4) as usize).copied();
                    if after.is_some_and(|a| a != b'/' as u16) {
                        break;
                    }
                    rest = rest[3..].to_vec();
                    self.index += 3;
                }

                let is_end = self.eos()
                    || (self.parens > 0
                        && rest == s(&")".repeat(self.parens as usize))
                        && !self.extglobs.iter().any(|e| e.ty == ExtType::Negate));

                if prior_ty == Ty::Bos && self.eos() {
                    let gs = self.globstar();
                    let pt = &mut self.arena[prev];
                    pt.ty = Ty::Globstar;
                    pt.value.extend_from_slice(&value);
                    pt.output = Some(gs.clone());
                    self.output = gs;
                    continue;
                }

                let prior_prev_bos = prior != BOS && self.arena[self.arena[prior].prev].ty == Ty::Bos;
                if prior_ty == Ty::Slash && !prior_prev_bos && !after_star && is_end {
                    let n = self.arena[prior].out_str().len() + self.arena[prev].out_str().len();
                    self.output = drop_last(&self.output, n);
                    let prior_out = cat(&[&s("(?:"), &self.arena[prior].out_str()]);
                    self.arena[prior].output = Some(prior_out.clone());
                    let prev_out = cat(&[&self.globstar(), &s("|$)")]);
                    let pt = &mut self.arena[prev];
                    pt.ty = Ty::Globstar;
                    pt.output = Some(prev_out.clone());
                    pt.value.extend_from_slice(&value);
                    self.output.extend_from_slice(&prior_out);
                    self.output.extend_from_slice(&prev_out);
                    continue;
                }

                if prior_ty == Ty::Slash && !prior_prev_bos && rest.first() == Some(&(b'/' as u16)) {
                    let end = if rest.len() > 1 { "|$" } else { "" };
                    let n = self.arena[prior].out_str().len() + self.arena[prev].out_str().len();
                    self.output = drop_last(&self.output, n);
                    let prior_out = cat(&[&s("(?:"), &self.arena[prior].out_str()]);
                    self.arena[prior].output = Some(prior_out.clone());
                    let prev_out = cat(&[&self.globstar(), &s(self.c.slash_literal), &s("|"), &s(self.c.slash_literal), &s(end), &s(")")]);
                    let pt = &mut self.arena[prev];
                    pt.ty = Ty::Globstar;
                    pt.output = Some(prev_out.clone());
                    pt.value.extend_from_slice(&value);
                    self.output.extend_from_slice(&prior_out);
                    self.output.extend_from_slice(&prev_out);
                    self.advance();
                    self.push(Tok::new(Ty::Slash, s("/"), Some(Vec::new())));
                    continue;
                }

                if prior_ty == Ty::Bos && rest.first() == Some(&(b'/' as u16)) {
                    let out = cat(&[&s("(?:^|"), &s(self.c.slash_literal), &s("|"), &self.globstar(), &s(self.c.slash_literal), &s(")")]);
                    let pt = &mut self.arena[prev];
                    pt.ty = Ty::Globstar;
                    pt.value.extend_from_slice(&value);
                    pt.output = Some(out.clone());
                    self.output = out;
                    self.advance();
                    self.push(Tok::new(Ty::Slash, s("/"), Some(Vec::new())));
                    continue;
                }

                let n = self.arena[prev].out_str().len();
                self.output = drop_last(&self.output, n);
                let gs = self.globstar();
                let pt = &mut self.arena[prev];
                pt.ty = Ty::Globstar;
                pt.output = Some(gs.clone());
                pt.value.extend_from_slice(&value);
                self.output.extend_from_slice(&gs);
                continue;
            }

            let token = Tok::new(Ty::Star, value, Some(self.star.clone()));
            let pty = self.arena[prev].ty;
            if self.index == self.start || pty == Ty::Slash || pty == Ty::Dot {
                let add = if pty == Ty::Dot {
                    self.c.no_dot_slash
                } else if self.opts.dot {
                    self.c.no_dots_slash
                } else {
                    self.nodot()
                };
                self.output.extend_from_slice(&s(add));
                let mut o = self.arena[prev].out_str();
                o.extend_from_slice(&s(add));
                if !self.peek_is(1, '*') {
                    self.output.extend_from_slice(&s(self.c.one_char));
                    o.extend_from_slice(&s(self.c.one_char));
                }
                self.arena[prev].output = Some(o);
            }
            self.push(token);
        }

        while self.brackets > 0 {
            self.output = escape_last(&self.output, '[', None);
            self.decrement(Counter::Brackets);
        }
        while self.parens > 0 {
            self.output = escape_last(&self.output, '(', None);
            self.decrement(Counter::Parens);
        }
        while self.braces > 0 {
            self.output = escape_last(&self.output, '{', None);
            self.decrement(Counter::Braces);
        }

        let pty = self.arena[self.prev].ty;
        if pty == Ty::Star || pty == Ty::Bracket {
            self.push(Tok::new(Ty::MaybeSlash, Vec::new(), Some(s(&format!("{}?", self.c.slash_literal)))));
        }

        if self.backtrack {
            self.output = Vec::new();
            for &t in &self.tokens {
                self.output.extend_from_slice(self.arena[t].out());
            }
        }
        Ok(())
    }

    fn negate(&mut self) {
        let mut count = 1;
        while self.peek_is(1, '!') && (!self.peek_is(2, '(') || self.peek_is(3, '?')) {
            self.advance();
            self.start += 1;
            count += 1;
        }
        if count % 2 == 0 {
            return;
        }
        self.negated = true;
        self.start += 1;
    }
}

/// `/<([!=]|\w+>)/.test(rest)`
fn lookbehind_or_name(rest: &[u16]) -> bool {
    (0..rest.len()).any(|i| {
        rest[i] == b'<' as u16
            && match rest.get(i + 1) {
                Some(&c) if c == b'!' as u16 || c == b'=' as u16 => true,
                _ => {
                    let n = rest[i + 1..].iter().take_while(|&&c| is_word(c)).count();
                    n > 0 && rest.get(i + 1 + n) == Some(&(b'>' as u16))
                }
            }
    })
}

/// `parse.fastpaths`
fn fastpaths(input: &[u16], opts: &Opts) -> Result<Option<S>, String> {
    if input.len() > MAX_LENGTH {
        return Err(format!("Input length: {}, exceeds maximum allowed length: {MAX_LENGTH}", input.len()));
    }
    let input = replacement(input).unwrap_or_else(|| input.to_vec());
    let output = if starts_with(&input, "./") { input[2..].to_vec() } else { input };
    let c = opts.chars();
    Ok(fastpath_create(&output, opts, c).map(|source| s(&format!("{source}{}?", c.slash_literal))))
}

/// `create` inside `parse.fastpaths`.
fn fastpath_create(str: &[u16], opts: &Opts, c: &Chars) -> Option<String> {
    let nodot = if opts.dot { c.no_dots } else { c.no_dot };
    let slash_dot = if opts.dot { c.no_dots_slash } else { c.no_dot };
    let star = c.star;
    let globstar = format!("(?:(?:(?!{}{}).)*?)", c.start_anchor, if opts.dot { c.dots_slash } else { c.dot_literal });
    let (dl, sl, one) = (c.dot_literal, c.slash_literal, c.one_char);
    let known = match String::from_utf16(str).ok().as_deref() {
        Some("*") => Some(format!("{nodot}{one}{star}")),
        Some(".*") => Some(format!("{dl}{one}{star}")),
        Some("*.*") => Some(format!("{nodot}{star}{dl}{one}{star}")),
        Some("*/*") => Some(format!("{nodot}{star}{sl}{one}{slash_dot}{star}")),
        Some("**") => Some(format!("{nodot}{globstar}")),
        Some("**/*") => Some(format!("(?:{nodot}{globstar}{sl})?{slash_dot}{one}{star}")),
        Some("**/*.*") => Some(format!("(?:{nodot}{globstar}{sl})?{slash_dot}{star}{dl}{one}{star}")),
        Some("**/.*") => Some(format!("(?:{nodot}{globstar}{sl})?{dl}{one}{star}")),
        _ => None,
    };
    if known.is_some() {
        return known;
    }
    // /^(.*?)\.(\w+)$/: only the last dot can be followed by word characters alone
    let dot = str.iter().rposition(|&ch| ch == b'.' as u16)?;
    let ext = &str[dot + 1..];
    if ext.is_empty() || !ext.iter().all(|&ch| is_word(ch)) || str[..dot].iter().any(|&ch| is_line_terminator(ch)) {
        return None;
    }
    let source = fastpath_create(&str[..dot], opts, c)?;
    Some(format!("{source}{dl}{}", String::from_utf16_lossy(ext)))
}

/// One compiled glob: `picomatch(glob, options)`.
struct Matcher {
    glob: S,
    /// None when JavaScript couldn't compile the regex: picomatch then never matches.
    regex: Option<fancy_regex::Regex>,
}

/// `picomatch(globs, options)`: true when any glob matches.
pub struct Glob {
    matchers: Vec<Matcher>,
    windows: bool,
}

impl Glob {
    /// `picomatch(globs, { dot: true })` (Windows separators on Windows, as in Node).
    pub fn new<T: AsRef<str>>(globs: &[T]) -> Result<Glob, String> {
        Self::with(globs, true, cfg!(windows))
    }

    /// `picomatch(globs)` with no options: POSIX, no dotfiles.
    pub fn plain<T: AsRef<str>>(globs: &[T]) -> Result<Glob, String> {
        Self::with(globs, false, false)
    }

    pub fn with<T: AsRef<str>>(globs: &[T], dot: bool, windows: bool) -> Result<Glob, String> {
        let opts = Opts { dot, windows, fastpaths: true };
        let mut matchers = Vec::new();
        for g in globs {
            let glob = s(g.as_ref());
            if glob.is_empty() {
                return Err("Expected pattern to be a non-empty string".into());
            }
            let source = make_re(&glob, &opts)?;
            matchers.push(Matcher { regex: jsre::compile(&source)?, glob });
        }
        Ok(Glob { matchers, windows })
    }

    pub fn is_match(&self, input: &str) -> bool {
        if input.is_empty() {
            return false;
        }
        let units = s(input);
        let output = if self.windows { to_posix_slashes(&units) } else { units.clone() };
        let mapped = jsre::units16(&output);
        self.matchers
            .iter()
            .any(|m| units == m.glob || output == m.glob || m.regex.as_ref().is_some_and(|r| r.is_match(&mapped).unwrap_or(false)))
    }
}

/// `picomatch.makeRe` → the final regex source.
fn make_re(input: &[u16], opts: &Opts) -> Result<S, String> {
    let mut parsed = None;
    if input[0] == b'.' as u16 || input[0] == b'*' as u16 {
        parsed = fastpaths(input, opts)?.filter(|o| !o.is_empty()).map(|output| Parsed { output, negated: false });
    }
    let parsed = match parsed {
        Some(p) => p,
        None => parse(input, opts)?,
    };
    let mut source = cat(&[&s("^(?:"), &parsed.output, &s(")$")]);
    if parsed.negated {
        source = cat(&[&s("^(?!"), &source, &s(").*$")]);
    }
    Ok(source)
}

/// The regex source picomatch builds for a glob (for tests against picomatch itself).
pub fn regex_source(glob: &str, dot: bool, windows: bool) -> Result<String, String> {
    let opts = Opts { dot, windows, fastpaths: true };
    Ok(String::from_utf16_lossy(&make_re(&s(glob), &opts)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_like_picomatch() {
        let g = Glob::with(&["**/*.md", "docs/**", "LICENSE*"], true, false).unwrap();
        assert!(g.is_match("README.md"));
        assert!(g.is_match("a/.b/c.md"));
        assert!(g.is_match("docs/x/y.ts"));
        assert!(g.is_match("LICENSE-MIT"));
        assert!(!g.is_match("src/a.ts"));
        assert!(!g.is_match(""));
        let plain = Glob::plain(&["crates/*"]).unwrap();
        assert!(plain.is_match("crates/a"));
        assert!(!plain.is_match("crates/.a"));
        assert!(!plain.is_match("crates/a/b"));
    }

    #[test]
    fn builds_picomatch_sources() {
        assert_eq!(regex_source("*.js", false, false).unwrap(), r"^(?:(?!\.)(?=.)[^/]*?\.js\/?)$");
        assert_eq!(regex_source("!a", false, false).unwrap(), r"^(?!^(?:a)$).*$");
    }

    #[test]
    fn rejects_empty_patterns() {
        assert!(Glob::new(&[""]).is_err());
    }
}
