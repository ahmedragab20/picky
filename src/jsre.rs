//! Compiles JavaScript regular expression sources (Annex B syntax) into Rust regexes.
//!
//! picomatch builds JavaScript regex source at run time, so globs need a JS regex engine.
//! JavaScript matches UTF-16 code units, so patterns and inputs are compared in "unit space":
//! every UTF-16 code unit becomes one `char`, with surrogate units moved to the private-use
//! range at `SURROGATE_BASE` (see [`units`]). A source JavaScript would reject is an `Err`.

use std::borrow::Cow;

const SURROGATE_BASE: u32 = 0xF0000;
/// Line terminators, which `.` doesn't match.
const LINE_TERMINATORS: &[(u32, u32)] = &[(0x0A, 0x0A), (0x0D, 0x0D), (0x2028, 0x2029)];
const DIGIT: &[(u32, u32)] = &[(0x30, 0x39)];
const WORD: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];
const SPACE: &[(u32, u32)] = &[
    (0x09, 0x0D),
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
    (0xFEFF, 0xFEFF),
];
const WORD_CLASS: &str = "[0-9A-Z_a-z]";

/// A string in unit space: one char per UTF-16 code unit.
pub fn units(s: &str) -> Cow<'_, str> {
    if s.chars().all(|c| (c as u32) < 0x10000) {
        return Cow::Borrowed(s);
    }
    Cow::Owned(units16(&s.encode_utf16().collect::<Vec<_>>()))
}

/// UTF-16 code units in unit space.
pub fn units16(u: &[u16]) -> String {
    u.iter().map(|&c| unit_char(c as u32)).collect()
}

fn unit_char(u: u32) -> char {
    let cp = if (0xD800..=0xDFFF).contains(&u) { SURROGATE_BASE + (u - 0xD800) } else { u };
    char::from_u32(cp).expect("unit maps to a char")
}

/// Compiles a JavaScript regex source (given as UTF-16 code units) for matching unit-space strings.
/// `Ok(None)` when JavaScript would throw a SyntaxError; `Err` when the regex is valid JavaScript
/// this translation can't run (callers must not treat that as "never matches").
pub fn compile(src: &[u16]) -> Result<Option<fancy_regex::Regex>, String> {
    let Ok(out) = translate(src) else { return Ok(None) };
    fancy_regex::RegexBuilder::new(&out)
        .backtrack_limit(usize::MAX)
        .build()
        .map(Some)
        .map_err(|e| format!("unsupported pattern /{}/: {e}", String::from_utf16_lossy(src)))
}

/// True when `new RegExp(src)` would not throw.
pub fn is_valid(src: &[u16]) -> bool {
    translate(src).is_ok()
}

/// Translates a JavaScript regex source to fancy-regex syntax.
pub fn translate(src: &[u16]) -> Result<String, String> {
    translate_with(src, false, false)
}

/// Like [`translate`], with the `m` flag when `multiline`. `native` writes `\b` and `\B` for the `regex`
/// crate, whose ASCII word boundary is JavaScript's; fancy-regex can't switch Unicode off, so it gets lookarounds.
fn translate_with(src: &[u16], multiline: bool, native: bool) -> Result<String, String> {
    let (groups, names) = count_groups(src);
    let mut p = Parser { s: src, i: 0, out: String::new(), groups, names, open: 0, multiline, native };
    p.disjunction()?;
    if p.i < p.s.len() {
        // only a stray `)` stops the top-level disjunction early
        return Err("Unmatched ')'".into());
    }
    Ok(p.out)
}

struct Parser<'a> {
    s: &'a [u16],
    i: usize,
    out: String,
    /// Capturing groups in the whole pattern (a backreference may point forward).
    groups: usize,
    /// Named groups, in order, with their group numbers.
    names: Vec<(Vec<u16>, usize)>,
    /// Capturing groups opened so far.
    open: usize,
    /// The `m` flag: `^` and `$` also match at line terminators.
    multiline: bool,
    /// Writing for the `regex` crate (see [`translate_with`]).
    native: bool,
}

fn is(c: Option<u16>, ch: char) -> bool {
    c == Some(ch as u16)
}

fn is_digit(c: Option<u16>) -> bool {
    matches!(c, Some(0x30..=0x39))
}

fn hex_val(c: u16) -> Option<u32> {
    (c < 0x80).then(|| (c as u8 as char).to_digit(16)).flatten()
}

/// Group count and named groups, scanning the source the way the parser will.
fn count_groups(s: &[u16]) -> (usize, Vec<(Vec<u16>, usize)>) {
    let mut n = 0;
    let mut names = Vec::new();
    let mut i = 0;
    let mut in_class = false;
    while i < s.len() {
        let c = s[i];
        if c == b'\\' as u16 {
            i += 2;
            continue;
        }
        if in_class {
            if c == b']' as u16 {
                in_class = false;
            }
        } else if c == b'[' as u16 {
            in_class = true;
        } else if c == b'(' as u16 {
            if !is(s.get(i + 1).copied(), '?') {
                n += 1;
            } else if is(s.get(i + 2).copied(), '<') && !matches!(s.get(i + 3).copied(), Some(0x3D | 0x21)) {
                n += 1;
                let start = i + 3;
                let mut j = start;
                while j < s.len() && s[j] != b'>' as u16 {
                    j += 1;
                }
                names.push((s[start..j.min(s.len())].to_vec(), n));
            }
        }
        i += 1;
    }
    (n, names)
}

/// A set of code units as sorted, merged inclusive ranges.
#[derive(Clone, Default)]
struct Set(Vec<(u32, u32)>);

impl Set {
    fn of(r: &[(u32, u32)]) -> Self {
        Set(r.to_vec())
    }
    fn add(&mut self, lo: u32, hi: u32) {
        self.0.push((lo, hi));
    }
    fn union(&mut self, other: &Set) {
        self.0.extend_from_slice(&other.0);
    }
    fn normalized(&self) -> Vec<(u32, u32)> {
        let mut v = self.0.clone();
        v.sort();
        let mut out: Vec<(u32, u32)> = Vec::new();
        for (lo, hi) in v {
            match out.last_mut() {
                Some(last) if lo <= last.1 + 1 => last.1 = last.1.max(hi),
                _ => out.push((lo, hi)),
            }
        }
        out
    }
    fn negated(&self) -> Set {
        let mut out = Vec::new();
        let mut next = 0u32;
        for (lo, hi) in self.normalized() {
            if lo > next {
                out.push((next, lo - 1));
            }
            next = hi + 1;
        }
        if next <= 0xFFFF {
            out.push((next, 0xFFFF));
        }
        Set(out)
    }
    /// Emits the set as a Rust class over unit space.
    fn emit(&self) -> String {
        let ranges = self.normalized();
        if ranges.is_empty() {
            return "(?!)".into();
        }
        let mut s = String::from("[");
        for (lo, hi) in ranges {
            // surrogate units live elsewhere in unit space, so split ranges around them
            for (a, b) in [(lo, hi.min(0xD7FF)), (lo.max(0xD800), hi.min(0xDFFF)), (lo.max(0xE000), hi)] {
                if a <= b {
                    let (a, b) = (unit_char(a), unit_char(b));
                    if a == b {
                        s.push_str(&format!("\\x{{{:X}}}", a as u32));
                    } else {
                        s.push_str(&format!("\\x{{{:X}}}-\\x{{{:X}}}", a as u32, b as u32));
                    }
                }
            }
        }
        s.push(']');
        s
    }
}

fn literal(u: u32) -> String {
    let c = unit_char(u);
    if c.is_ascii_alphabetic() { c.to_string() } else { format!("\\x{{{:X}}}", c as u32) }
}

enum ClassAtom {
    Unit(u32),
    Set(Set),
}

impl Parser<'_> {
    fn peek(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }
    fn at(&self, k: usize) -> Option<u16> {
        self.s.get(self.i + k).copied()
    }

    /// Ok(true) when what was parsed never consumes input (assertions and empty groups only).
    fn disjunction(&mut self) -> Result<bool, String> {
        let mut zero_width = true;
        loop {
            zero_width &= self.alternative()?;
            if is(self.peek(), '|') {
                self.i += 1;
                self.out.push('|');
            } else {
                return Ok(zero_width);
            }
        }
    }

    fn alternative(&mut self) -> Result<bool, String> {
        let mut zero_width = true;
        while let Some(c) = self.peek() {
            if c == b'|' as u16 || c == b')' as u16 {
                break;
            }
            zero_width &= self.term()?;
        }
        Ok(zero_width)
    }

    /// `{n}`, `{n,}` or `{n,m}` at the cursor: (min, max, length).
    fn braced(&self) -> Option<(u64, Option<u64>, usize)> {
        if !is(self.peek(), '{') {
            return None;
        }
        let mut j = self.i + 1;
        let num = |j: &mut usize| -> Option<u64> {
            let start = *j;
            let mut v: u64 = 0;
            while is_digit(self.s.get(*j).copied()) {
                v = v.saturating_mul(10).saturating_add((self.s[*j] - 0x30) as u64);
                *j += 1;
            }
            (*j > start).then_some(v)
        };
        let min = num(&mut j)?;
        let max = if is(self.s.get(j).copied(), ',') {
            j += 1;
            if is(self.s.get(j).copied(), '}') { None } else { Some(num(&mut j)?) }
        } else {
            Some(min)
        };
        is(self.s.get(j).copied(), '}').then_some((min, max, j + 1 - self.i))
    }

    /// Parses one term; Ok(true) when it never consumes input.
    fn term(&mut self) -> Result<bool, String> {
        let c = self.peek().unwrap();
        // assertions that can't take a quantifier
        let assertion = match c {
            0x5E if self.multiline => Some(r"(?:^|(?<=[\n\r\x{2028}\x{2029}]))".to_string()),
            0x24 if self.multiline => Some(r"(?:$|(?=[\n\r\x{2028}\x{2029}]))".to_string()),
            0x5E => Some("^".to_string()),
            0x24 => Some("$".to_string()),
            0x5C if is(self.at(1), 'b') && self.native => Some(r"(?-u:\b)".to_string()),
            0x5C if is(self.at(1), 'B') && self.native => Some(r"(?-u:\B)".to_string()),
            0x5C if is(self.at(1), 'b') => Some(format!("(?:(?<={WORD_CLASS})(?!{WORD_CLASS})|(?<!{WORD_CLASS})(?={WORD_CLASS}))")),
            0x5C if is(self.at(1), 'B') => Some(format!("(?:(?<={WORD_CLASS})(?={WORD_CLASS})|(?<!{WORD_CLASS})(?!{WORD_CLASS}))")),
            _ => None,
        };
        if let Some(a) = assertion {
            self.i += if c == 0x5C { 2 } else { 1 };
            self.out.push_str(&a);
            return self.no_quantifier().map(|()| true);
        }
        let mut zero_width = false;
        let atom = match c {
            0x28 => {
                // (
                if is(self.at(1), '?') {
                    match (self.at(2), self.at(3)) {
                        (Some(0x3A), _) => {
                            let (g, zw) = self.group(3, "(?:")?;
                            zero_width = zw;
                            g
                        }
                        (Some(0x3D), _) => {
                            zero_width = true;
                            self.group(3, "(?=")?.0
                        }
                        (Some(0x21), _) => {
                            zero_width = true;
                            self.group(3, "(?!")?.0
                        }
                        (Some(0x3C), Some(0x3D)) => {
                            let (g, _) = self.group(4, "(?<=")?;
                            self.out.push_str(&g);
                            return self.no_quantifier().map(|()| true);
                        }
                        (Some(0x3C), Some(0x21)) => {
                            let (g, _) = self.group(4, "(?<!")?;
                            self.out.push_str(&g);
                            return self.no_quantifier().map(|()| true);
                        }
                        (Some(0x3C), _) => {
                            let start = self.i + 3;
                            let end = (start..self.s.len()).find(|&j| self.s[j] == b'>' as u16);
                            let Some(end) = end else { return Err("Invalid capture group name".into()) };
                            let name = &self.s[start..end];
                            if !valid_group_name(name) || self.names.iter().filter(|(n, _)| n == name).count() > 1 {
                                return Err("Invalid capture group name".into());
                            }
                            self.open += 1;
                            let (g, zw) = self.group(end + 1 - self.i, "(")?;
                            zero_width = zw;
                            g
                        }
                        _ => return Err("Invalid group".into()),
                    }
                } else {
                    self.open += 1;
                    let (g, zw) = self.group(1, "(")?;
                    zero_width = zw;
                    g
                }
            }
            0x5B => self.class()?,
            0x2E => {
                self.i += 1;
                Set::of(LINE_TERMINATORS).negated().emit()
            }
            0x2A | 0x2B | 0x3F => return Err("Nothing to repeat".into()),
            0x7B if self.braced().is_some() => return Err("Nothing to repeat".into()),
            0x5C => self.atom_escape()?,
            _ => {
                self.i += 1;
                literal(c as u32)
            }
        };
        let quant = self.quantifier()?;
        match quant {
            None => self.out.push_str(&atom),
            // An atom that never consumes input (a lookahead, which Annex B lets repeat, or an empty
            // group) holds once when it must repeat. When it may repeat zero times, JavaScript's
            // empty-iteration check always takes zero: its groups stay unset, but keep their numbers.
            Some((min, _)) if zero_width => {
                if min > 0 {
                    self.out.push_str(&atom);
                } else {
                    self.out.push_str("(?:|(?!)");
                    self.out.push_str(&atom);
                    self.out.push(')');
                }
            }
            Some((_, q)) => {
                self.out.push_str("(?:");
                self.out.push_str(&atom);
                self.out.push(')');
                self.out.push_str(&q);
            }
        }
        Ok(zero_width)
    }

    fn no_quantifier(&self) -> Result<(), String> {
        match self.peek() {
            Some(0x2A | 0x2B | 0x3F) => Err("Nothing to repeat".into()),
            Some(0x7B) if self.braced().is_some() => Err("Nothing to repeat".into()),
            _ => Ok(()),
        }
    }

    fn quantifier(&mut self) -> Result<Option<(u64, String)>, String> {
        let (min, mut q) = match self.peek() {
            Some(0x2A) => (0, "*".to_string()),
            Some(0x2B) => (1, "+".to_string()),
            Some(0x3F) => (0, "?".to_string()),
            Some(0x7B) => match self.braced() {
                Some((min, max, len)) => {
                    if max.is_some_and(|m| m < min) {
                        return Err("numbers out of order in {} quantifier".into());
                    }
                    self.i += len - 1;
                    let q = match max {
                        Some(m) if m == min => format!("{{{min}}}"),
                        Some(m) => format!("{{{min},{m}}}"),
                        None => format!("{{{min},}}"),
                    };
                    (min, q)
                }
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
        self.i += 1;
        if is(self.peek(), '?') {
            self.i += 1;
            q.push('?');
        }
        Ok(Some((min, q)))
    }

    /// Parses a group body after an opener of `len` units; returns the emitted group, and
    /// whether its body never consumes input.
    fn group(&mut self, len: usize, open: &str) -> Result<(String, bool), String> {
        self.i += len;
        let saved = std::mem::take(&mut self.out);
        let zero_width = self.disjunction()?;
        if !is(self.peek(), ')') {
            return Err("Unterminated group".into());
        }
        self.i += 1;
        let body = std::mem::replace(&mut self.out, saved);
        Ok((format!("{open}{body})"), zero_width))
    }

    fn atom_escape(&mut self) -> Result<String, String> {
        let Some(c) = self.at(1) else { return Err("\\ at end of pattern".into()) };
        match c as u8 {
            b'1'..=b'9' if c < 0x80 => {
                let mut j = self.i + 1;
                let mut n: u64 = 0;
                while is_digit(self.s.get(j).copied()) {
                    n = n.saturating_mul(10).saturating_add((self.s[j] - 0x30) as u64);
                    j += 1;
                }
                if n as usize <= self.groups {
                    self.i = j;
                    return Ok(backreference(n as usize));
                }
                self.i += 1;
                if c >= b'8' as u16 {
                    self.i += 1;
                    return Ok(literal(c as u32));
                }
                Ok(literal(self.octal()))
            }
            b'k' if c < 0x80 && !self.names.is_empty() => {
                let start = self.i + 3;
                if !is(self.at(2), '<') {
                    return Err("Invalid named reference".into());
                }
                let end = (start..self.s.len()).find(|&j| self.s[j] == b'>' as u16);
                let Some(end) = end else { return Err("Invalid named reference".into()) };
                let name = &self.s[start..end];
                let Some(&(_, n)) = self.names.iter().find(|(n, _)| n == name) else {
                    return Err("Invalid named capture referenced".into());
                };
                self.i = end + 1;
                Ok(backreference(n))
            }
            _ => match self.char_escape(false)? {
                ClassAtom::Unit(u) => Ok(literal(u)),
                ClassAtom::Set(s) => Ok(s.emit()),
            },
        }
    }

    /// Legacy octal escape at the cursor (on the first digit).
    fn octal(&mut self) -> u32 {
        fn digit(c: Option<u16>) -> Option<u32> {
            c.filter(|c| (0x30..=0x37).contains(c)).map(|c| (c - 0x30) as u32)
        }
        let first = digit(self.peek()).expect("octal digit");
        self.i += 1;
        let Some(second) = digit(self.peek()) else { return first };
        self.i += 1;
        if first >= 4 {
            return first * 8 + second;
        }
        let Some(third) = digit(self.peek()) else { return first * 8 + second };
        self.i += 1;
        first * 64 + second * 8 + third
    }

    /// A character escape at the cursor (on the `\`), shared by atoms and classes.
    fn char_escape(&mut self, in_class: bool) -> Result<ClassAtom, String> {
        let Some(c) = self.at(1) else { return Err("\\ at end of pattern".into()) };
        self.i += 2;
        let ascii = if c < 0x80 { c as u8 } else { 0 };
        let set = |r: &[(u32, u32)], neg: bool| {
            let s = Set::of(r);
            ClassAtom::Set(if neg { s.negated() } else { s })
        };
        Ok(match ascii {
            b'd' => set(DIGIT, false),
            b'D' => set(DIGIT, true),
            b'w' => set(WORD, false),
            b'W' => set(WORD, true),
            b's' => set(SPACE, false),
            b'S' => set(SPACE, true),
            b'f' => ClassAtom::Unit(0x0C),
            b'n' => ClassAtom::Unit(0x0A),
            b'r' => ClassAtom::Unit(0x0D),
            b't' => ClassAtom::Unit(0x09),
            b'v' => ClassAtom::Unit(0x0B),
            b'b' if in_class => ClassAtom::Unit(0x08),
            b'c' => match self.peek() {
                Some(l) if (l as u8).is_ascii_alphabetic() && l < 0x80 => {
                    self.i += 1;
                    ClassAtom::Unit((l % 32) as u32)
                }
                Some(l) if in_class && l < 0x80 && ((l as u8).is_ascii_digit() || l == b'_' as u16) => {
                    self.i += 1;
                    ClassAtom::Unit((l % 32) as u32)
                }
                _ => {
                    // `\c` without a control letter is a literal backslash
                    self.i -= 1;
                    ClassAtom::Unit(0x5C)
                }
            },
            b'0' if !is_digit(self.peek()) => ClassAtom::Unit(0),
            b'0'..=b'7' => {
                self.i -= 1;
                ClassAtom::Unit(self.octal())
            }
            b'x' => match (self.peek().and_then(hex_val), self.at(1).and_then(hex_val)) {
                (Some(a), Some(b)) => {
                    self.i += 2;
                    ClassAtom::Unit(a * 16 + b)
                }
                _ => ClassAtom::Unit(b'x' as u32),
            },
            b'u' => {
                let digits: Option<Vec<u32>> = (0..4).map(|k| self.at(k).and_then(hex_val)).collect();
                match digits {
                    Some(d) => {
                        self.i += 4;
                        ClassAtom::Unit(d.iter().fold(0, |acc, v| acc * 16 + v))
                    }
                    None => ClassAtom::Unit(b'u' as u32),
                }
            }
            _ => ClassAtom::Unit(c as u32),
        })
    }

    fn class(&mut self) -> Result<String, String> {
        self.i += 1;
        let negate = is(self.peek(), '^');
        if negate {
            self.i += 1;
        }
        let mut set = Set::default();
        loop {
            let Some(c) = self.peek() else { return Err("Unterminated character class".into()) };
            if c == b']' as u16 {
                self.i += 1;
                break;
            }
            let lo = self.class_atom()?;
            if is(self.peek(), '-') && self.at(1).is_some_and(|c| c != b']' as u16) {
                self.i += 1;
                let hi = self.class_atom()?;
                match (lo, hi) {
                    (ClassAtom::Unit(a), ClassAtom::Unit(b)) => {
                        if a > b {
                            return Err("Range out of order in character class".into());
                        }
                        set.add(a, b);
                    }
                    // Annex B: a range with a class escape is a union that includes `-`
                    (a, b) => {
                        for atom in [a, b] {
                            match atom {
                                ClassAtom::Unit(u) => set.add(u, u),
                                ClassAtom::Set(s) => set.union(&s),
                            }
                        }
                        set.add(0x2D, 0x2D);
                    }
                }
            } else {
                match lo {
                    ClassAtom::Unit(u) => set.add(u, u),
                    ClassAtom::Set(s) => set.union(&s),
                }
            }
        }
        Ok(if negate { set.negated().emit() } else { set.emit() })
    }

    fn class_atom(&mut self) -> Result<ClassAtom, String> {
        let c = self.peek().unwrap();
        if c != b'\\' as u16 {
            self.i += 1;
            return Ok(ClassAtom::Unit(c as u32));
        }
        match self.at(1) {
            // in a class, `\8` and `\9` are identity escapes and other digits are octal
            Some(d @ 0x38..=0x39) => {
                self.i += 2;
                Ok(ClassAtom::Unit(d as u32))
            }
            Some(0x31..=0x37) => {
                self.i += 1;
                Ok(ClassAtom::Unit(self.octal()))
            }
            Some(0x2D) => {
                self.i += 2;
                Ok(ClassAtom::Unit(0x2D))
            }
            _ => self.char_escape(true),
        }
    }
}

/// A unit-space string back as text; a lone surrogate becomes U+FFFD.
pub fn from_units(s: &str) -> Cow<'_, str> {
    let base = char::from_u32(SURROGATE_BASE).unwrap();
    let top = char::from_u32(SURROGATE_BASE + 0x7FF).unwrap();
    if !s.chars().any(|c| (base..=top).contains(&c)) {
        return Cow::Borrowed(s);
    }
    let u: Vec<u16> = s
        .chars()
        .flat_map(|c| {
            let cp = c as u32;
            if (SURROGATE_BASE..=SURROGATE_BASE + 0x7FF).contains(&cp) {
                vec![(cp - SURROGATE_BASE + 0xD800) as u16]
            } else {
                let mut buf = [0u16; 2];
                c.encode_utf16(&mut buf).to_vec()
            }
        })
        .collect();
    Cow::Owned(String::from_utf16_lossy(&u))
}

/// A regex literal from picky's own source, run with JavaScript semantics on unit-space strings
/// (see [`units`]). Flags: `i`, `m`; `g` only changes how the caller iterates.
pub struct JsRegex(Engine);

enum Engine {
    /// No lookaround or backreference: the `regex` crate, which never backtracks.
    Native(regex::Regex),
    /// fancy-regex backtracks, trying every start position in turn: slow on long inputs.
    Fancy(fancy_regex::Regex),
}

/// One match: offsets are byte offsets into the unit-space haystack.
pub struct JsMatch<'t> {
    pub start: usize,
    pub end: usize,
    groups: Vec<Option<&'t str>>,
}

impl<'t> JsMatch<'t> {
    /// Group `i` (0 is the whole match), or None when it didn't participate.
    pub fn get(&self, i: usize) -> Option<&'t str> {
        self.groups.get(i).copied().flatten()
    }
    /// Group `i`, or "" when it didn't participate.
    pub fn str(&self, i: usize) -> &'t str {
        self.get(i).unwrap_or("")
    }
}

impl JsRegex {
    /// Panics on an invalid pattern: these are literals, checked by the tests.
    pub fn new(src: &str, flags: &str) -> JsRegex {
        let units: Vec<u16> = src.encode_utf16().collect();
        let (multiline, case) = (flags.contains('m'), if flags.contains('i') { "(?i)" } else { "" });
        // The regex crate rejects lookarounds and backreferences; everything else means the same to both engines.
        if let Ok(out) = translate_with(&units, multiline, true)
            && let Ok(re) = regex::Regex::new(&format!("{case}{out}"))
        {
            return JsRegex(Engine::Native(re));
        }
        let out = translate_with(&units, multiline, false).unwrap_or_else(|e| panic!("invalid regex /{src}/: {e}"));
        let re = fancy_regex::RegexBuilder::new(&format!("{case}{out}"))
            .backtrack_limit(usize::MAX)
            .build()
            .unwrap_or_else(|e| panic!("unsupported regex /{src}/: {e}"));
        JsRegex(Engine::Fancy(re))
    }

    /// `re.test(s)`.
    pub fn test(&self, u: &str) -> bool {
        match &self.0 {
            Engine::Native(re) => re.is_match(u),
            Engine::Fancy(re) => re.is_match(u).expect("no backtrack limit"),
        }
    }

    /// `re.exec(s)` from the start (no `lastIndex`).
    pub fn exec<'t>(&self, u: &'t str) -> Option<JsMatch<'t>> {
        self.exec_at(u, 0)
    }

    /// The first match starting at or after byte offset `pos`, like `exec` with `lastIndex = pos`.
    pub fn exec_at<'t>(&self, u: &'t str, pos: usize) -> Option<JsMatch<'t>> {
        let groups: Vec<Option<(usize, &'t str)>> = match &self.0 {
            // Without groups, the match alone: no capture slots to fill.
            Engine::Native(re) if re.captures_len() == 1 => {
                let m = re.find_at(u, pos)?;
                vec![Some((m.start(), m.as_str()))]
            }
            Engine::Native(re) => {
                let caps = re.captures_at(u, pos)?;
                caps.iter().map(|m| m.map(|m| (m.start(), m.as_str()))).collect()
            }
            Engine::Fancy(re) => {
                let caps = re.captures_from_pos(u, pos).expect("no backtrack limit")?;
                (0..caps.len()).map(|i| caps.get(i).map(|m| (m.start(), m.as_str()))).collect()
            }
        };
        let (start, whole) = groups[0].expect("group 0");
        Some(JsMatch { start, end: start + whole.len(), groups: groups.iter().map(|g| g.map(|(_, s)| s)).collect() })
    }

    /// `s.matchAll(re)`: every match, an empty match advancing by one unit.
    pub fn match_all<'t>(&self, u: &'t str) -> Vec<JsMatch<'t>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= u.len() {
            let Some(m) = self.exec_at(u, pos) else { break };
            pos = if m.end == m.start { m.end + u[m.end..].chars().next().map_or(1, char::len_utf8) } else { m.end };
            out.push(m);
        }
        out
    }

    /// `s.search(re)`: byte offset of the first match.
    pub fn search(&self, u: &str) -> Option<usize> {
        self.exec(u).map(|m| m.start)
    }

    /// `s.replace(re, f)` with the `g` flag.
    pub fn replace_all(&self, u: &str, mut f: impl FnMut(&JsMatch) -> String) -> String {
        let mut out = String::new();
        let mut last = 0;
        for m in self.match_all(u) {
            out.push_str(&u[last..m.start]);
            out.push_str(&f(&m));
            last = m.end;
        }
        out.push_str(&u[last..]);
        out
    }

    /// `s.replace(re, f)` without the `g` flag.
    pub fn replace_first(&self, u: &str, f: impl FnOnce(&JsMatch) -> String) -> String {
        match self.exec(u) {
            Some(m) => format!("{}{}{}", &u[..m.start], f(&m), &u[m.end..]),
            None => u.to_string(),
        }
    }
}

/// RegExpIdentifierName, ASCII-only approximation plus `$`: names only matter for validity.
fn valid_group_name(name: &[u16]) -> bool {
    let Some((&first, rest)) = name.split_first() else { return false };
    let start = |c: u16| c >= 0x80 || (c as u8).is_ascii_alphabetic() || c == b'$' as u16 || c == b'_' as u16;
    start(first) && rest.iter().all(|&c| start(c) || (c as u8).is_ascii_digit() && c < 0x80)
}

/// `\n`: a group that hasn't matched (yet, or at all) matches the empty string in JavaScript,
/// where the regex engine would fail.
fn backreference(n: usize) -> String {
    format!("(?({n})\\{n}|)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(src: &str, input: &str) -> Option<bool> {
        let re = compile(&src.encode_utf16().collect::<Vec<_>>()).unwrap()?;
        Some(re.is_match(units(input).as_ref()).unwrap())
    }

    #[test]
    fn matches_like_javascript() {
        assert_eq!(m(r"^(?:(?!\.)(?=.)[^/]*?\.js)$", "a.js"), Some(true));
        assert_eq!(m(r"^(?:(?!\.)(?=.)[^/]*?\.js)$", ".a.js"), Some(false));
        assert_eq!(m(r"^\w+$", "é"), Some(false));
        assert_eq!(m(r"^.$", "\n"), Some(false));
        assert_eq!(m(r"^..$", "😀"), Some(true));
        assert_eq!(m(r"^.$", "😀"), Some(false));
        assert_eq!(m(r"^[\d-z]+$", "1-z"), Some(true));
        assert_eq!(m(r"^a{2}$", "aa"), Some(true));
        assert_eq!(m(r"^a{,2}$", "a{,2}"), Some(true));
        assert_eq!(m(r"^\u{41}$", "uuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuuu"), Some(true));
        assert_eq!(m(r"^\1$", "\u{1}"), Some(true));
        assert_eq!(m(r"^[]$", ""), Some(false));
        assert_eq!(m(r"\bfoo\b", "a foo b"), Some(true));
        assert_eq!(m(r"\bfoo\b", "éfooé"), Some(true));
        assert_eq!(m(r"^\cJ\c1$", "\n\\c1"), Some(true));
        assert_eq!(m(r"^(?=a)*b$", "b"), Some(true));
        // a group that hasn't matched: its backreference matches the empty string
        assert_eq!(m(r"^(a)?b\1$", "b"), Some(true));
        assert_eq!(m(r"^(a)?b\1$", "ab"), Some(false));
        assert_eq!(m(r"^\2(a)(b)$", "ab"), Some(true));
    }

    /// Quantified atoms that never consume input: JavaScript's empty-iteration check, checked
    /// against `new RegExp(src).exec(input)` in Node.
    #[test]
    fn repeats_zero_width_atoms_like_javascript() {
        // (pattern, input, expected groups; None when it doesn't match)
        type Case<'a> = (&'a str, &'a str, Option<&'a [Option<&'a str>]>);
        let cases: [Case; 10] = [
            (r"^(?:)+c$", "c", Some(&[Some("c")])),
            (r"^()+c$", "c", Some(&[Some("c"), Some("")])),
            (r"^()*c$", "c", Some(&[Some("c"), None])),
            (r"^(?=(a))*a", "a", Some(&[Some("a"), None])),
            (r"^(?=(a))+a", "a", Some(&[Some("a"), Some("a")])),
            (r"(?:(?<=a))+b", "ab", Some(&[Some("b")])),
            (r"^(?:(?:)(?:)){2}x$", "x", Some(&[Some("x")])),
            (r"^(?:(?=(a)))?(a)\2$", "aa", Some(&[Some("aa"), None, Some("a")])),
            (r"^(?:(?=(a)))?(a)\1$", "a", Some(&[Some("a"), None, Some("a")])),
            (r"^(?:\b)*x", "x", Some(&[Some("x")])),
        ];
        for (src, input, want) in cases {
            let re = JsRegex::new(src, "");
            let got =
                re.exec(&units(input)).map(|m| (0..want.map_or(0, <[_]>::len)).map(|i| m.get(i).map(str::to_string)).collect::<Vec<_>>());
            let want = want.map(|w| w.iter().map(|g| g.map(str::to_string)).collect::<Vec<_>>());
            assert_eq!(got, want, "{src}");
        }
    }

    #[test]
    fn rejects_what_javascript_rejects() {
        for src in [
            "[z-a]",
            "(",
            ")",
            "a**",
            "*",
            "a{2,1}",
            "[a",
            "\\",
            "(?<1a>x)",
            "^*",
            "(?<=a)+",
            "a{1}{2}",
            "{2}",
            "(?<a>x)\\k<b>",
            "(?<a>x)(?<a>y)",
        ] {
            assert_eq!(m(src, "a"), None, "{src}");
        }
    }
}
