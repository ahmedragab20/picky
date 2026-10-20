//! JavaScript semantics picky's output depends on: number formatting and parsing, string
//! trimming, UTF-16 ordering and `localeCompare`.

use std::cmp::Ordering;

/// `String(x)` for a number.
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if x < 0.0 {
        return format!("-{}", number_to_string(-x));
    }
    // Shortest round-trip digits, as `d.ddde±x`.
    let e = format!("{x:e}");
    let (mantissa, exp) = e.split_once('e').expect("exponent form");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    let n = exp.parse::<i64>().expect("exponent") + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        if k == 1 { format!("{digits}e{sign}{}", e.abs()) } else { format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs()) }
    }
}

/// JavaScript's WhiteSpace and LineTerminator code points (what `trim` and `\s` cover).
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// `\s` as a regex class, for porting JavaScript regexes to the `regex` crate.
pub const SPACE_CLASS: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

pub fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn trim_start(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `Number(s)` for a string.
pub fn parse_number(s: &str) -> f64 {
    let t = trim(s);
    if t.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            if rest.is_empty() || !rest.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return rest.chars().fold(0.0, |acc, c| acc * radix as f64 + c.to_digit(radix).unwrap() as f64);
        }
    }
    let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t);
    if unsigned == "Infinity" {
        return if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    // StrDecimalLiteral: digits [. digits] | . digits, then an optional exponent
    let b = unsigned.as_bytes();
    let mut i = 0;
    let int = b.iter().take_while(|c| c.is_ascii_digit()).count();
    i += int;
    let mut frac = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        frac = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        i += frac;
    }
    if int == 0 && frac == 0 {
        return f64::NAN;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exp = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp == 0 {
            return f64::NAN;
        }
        i += exp;
    }
    if i != b.len() {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// `x.toFixed(digits)`: exact decimal value, ties rounded up.
pub fn to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return number_to_string(x);
    }
    let negative = x < 0.0;
    // Every finite double has an exact decimal expansion of at most 1074 fractional digits.
    let exact = format!("{:.1100}", x.abs());
    let (int, frac) = exact.split_once('.').expect("fraction");
    let mut kept: Vec<u8> = int.bytes().chain(frac.bytes().take(digits)).collect();
    let round_up = frac.as_bytes().get(digits).is_some_and(|d| *d >= b'5');
    if round_up {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                break;
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    let int_len = kept.len() - digits;
    let mut out = String::from_utf8(kept[..int_len].to_vec()).unwrap();
    if digits > 0 {
        out.push('.');
        out.push_str(std::str::from_utf8(&kept[int_len..]).unwrap());
    }
    let is_zero = out.bytes().all(|c| c == b'0' || c == b'.');
    if negative && !is_zero { format!("-{out}") } else { out }
}

/// JavaScript's default string order: by UTF-16 code units.
pub fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Sorts like `array.sort()` on strings.
pub fn sort_strings(v: &mut [String]) {
    v.sort_by(|a, b| utf16_cmp(a, b));
}

/// Primary order of the non-ignorable ASCII characters under the CLDR root collation (what
/// `localeCompare` uses in Node with ICU); letters share a weight with their capital.
const ASCII_ORDER: &[u8] = b"\t\n\x0B\x0C\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

fn collation_key(c: char) -> Option<(u32, u8)> {
    if c.is_ascii() {
        let lower = c.to_ascii_lowercase() as u8;
        let primary = ASCII_ORDER.iter().position(|&o| o == lower)? as u32;
        return Some((primary, u8::from(c.is_ascii_uppercase())));
    }
    // Outside ASCII: after every ASCII character, by code point (an approximation of ICU).
    Some((0x100 + c as u32, 0))
}

/// `a.localeCompare(b)` for the root locale: primary weights first, then lowercase before uppercase.
/// Exact for ASCII; other characters sort after ASCII by code point.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let ka: Vec<(u32, u8)> = a.chars().filter_map(collation_key).collect();
    let kb: Vec<(u32, u8)> = b.chars().filter_map(collation_key).collect();
    let primary = ka.iter().map(|k| k.0).cmp(kb.iter().map(|k| k.0));
    primary.then_with(|| ka.iter().map(|k| k.1).cmp(kb.iter().map(|k| k.1)))
}

/// `s.replace(pattern, replacement)` with a string pattern: the first occurrence, with the
/// replacement's `$$`, `$&`, `` $` `` and `$'` substitutions.
pub fn replace_str(s: &str, pattern: &str, replacement: &str) -> String {
    let Some(at) = s.find(pattern) else { return s.to_string() };
    let (before, after) = (&s[..at], &s[at + pattern.len()..]);
    let mut out = String::from(before);
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('$') => out.push('$'),
            Some('&') => out.push_str(pattern),
            Some('`') => out.push_str(before),
            Some('\'') => out.push_str(after),
            _ => {
                out.push('$');
                continue;
            }
        }
        chars.next();
    }
    out.push_str(after);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_like_javascript() {
        let cases = [
            (1.0, "1"),
            (-0.0, "0"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (123.456, "123.456"),
            (f64::NAN, "NaN"),
            (-f64::INFINITY, "-Infinity"),
            (2f64.powi(53) + 2.0, "9007199254740994"),
        ];
        for (x, want) in cases {
            assert_eq!(number_to_string(x), want, "{x}");
        }
    }

    #[test]
    fn parses_numbers_like_javascript() {
        assert_eq!(parse_number(" 12 "), 12.0);
        assert_eq!(parse_number(""), 0.0);
        assert_eq!(parse_number("0x10"), 16.0);
        assert_eq!(parse_number("1e3"), 1000.0);
        assert_eq!(parse_number(".5"), 0.5);
        assert_eq!(parse_number("5."), 5.0);
        assert_eq!(parse_number("-Infinity"), f64::NEG_INFINITY);
        for nan in ["abc", "inf", "1e", "-0x1", "1_0", "."] {
            assert!(parse_number(nan).is_nan(), "{nan}");
        }
    }

    #[test]
    fn rounds_like_to_fixed() {
        let cases = [(0.125, "0.13"), (0.145, "0.14"), (0.005, "0.01"), (0.995, "0.99"), (1.0, "1.00"), (0.0, "0.00"), (3.8, "3.80")];
        for (x, want) in cases {
            assert_eq!(to_fixed(x, 2), want, "{x}");
        }
        assert_eq!(to_fixed(3.85, 1), "3.9");
        assert_eq!(to_fixed(0.05, 1), "0.1");
    }

    #[test]
    fn compares_like_locale_compare() {
        let mut v = ["b", "a", "B", "A", "_", "-", "1", "aa", "a_b", "a-b", "a.b", "a/b", "aB", "ab", "Ab"];
        v.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(v.join(" "), "_ - 1 a A a_b a-b a.b a/b aa ab aB Ab b B");
        assert_eq!(locale_compare("a\u{1}b", "ab"), Ordering::Equal);
    }

    #[test]
    fn replaces_like_string_replace() {
        assert_eq!(replace_str("src/*/x/*", "*", "a"), "src/a/x/*");
        assert_eq!(replace_str("src/*", "*", "$$-$&-$`-$'-$1-$<n>-$"), "src/$-*-src/--$1-$<n>-$");
        assert_eq!(replace_str("none", "*", "a"), "none");
    }
}
