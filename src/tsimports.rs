//! Port of TypeScript 5.9.3 `ts.preProcessFile(text, /*readImportFiles*/ true,
//! /*detectJavaScriptImports*/ true).importedFiles`, plus the parts of the TS
//! scanner it uses. Std-only.
//!
//! Source of truth: `typescript.js` 5.9.3, `function preProcessFile` and the
//! module-level `scanner` it drives (`createScanner(Latest, skipTrivia = true)`,
//! standard language variant, no `onError`).
//!
//! What this port keeps exactly:
//!
//! * The scanner runs over UTF-16 code units, like the JS string it came from.
//!   `codePointAt` semantics are used wherever the JS uses them.
//! * Identifier start/part tables are the `unicodeESNext*` arrays that the
//!   scanner picks for `ScriptTarget.Latest` (the ES5 tables are never used by
//!   this scanner and are not included).
//! * `preProcessFile` never calls `reScanSlashToken`, so a regular expression
//!   literal is *not* recognised: its characters are scanned as ordinary
//!   tokens (a quote inside a regex starts a string, and so on).
//! * A U+FFFD at a token boundary makes the scanner jump to the end of the text
//!   (TS treats it as "file appears to be binary").
//! * Quirks are kept: for example `scanIdentifierParts` drops the plain
//!   characters between the start of the parts (or the previous escape) and a
//!   following `\u{...}` escape, so `\u{69}m\u{70}ort` is not `import`.
//! * A BOM, `\r`, ` `/` `, NBSP and the other Unicode spaces are
//!   trivia exactly as in the TS scanner; `<<<<<<<`/`=======`/`>>>>>>>`
//!   conflict markers and a `#!` line at offset 0 are skipped like TS does.
//!
//! What differs (none of it changes the specifiers returned for text the JS
//! function can process):
//!
//! * Token values of numeric literals are not computed (only the end position
//!   matters to the walker).
//! * The JS function throws `RangeError: Maximum call stack size exceeded` for
//!   a single `\u{...}` or `0x...` run of about 123k or more hex digits
//!   (`String.fromCharCode(...array)`, measured on Node 26); this port returns
//!   the specifiers instead.
//! * [`imported_files`] converts the UTF-16 results with
//!   `String::from_utf16_lossy`, so a lone surrogate (which can only come from
//!   an escape such as `"\uD800"` inside a specifier) becomes U+FFFD. Use
//!   [`imported_files_utf16`] to get the exact code units.
//!
//! Input is expected to be `String::from_utf8_lossy(bytes)`: that matches
//! Node's `readFileSync(path, "utf8")`, and a leading BOM stays in the text as
//! U+FEFF (the scanner treats it as whitespace).

/// `ts.preProcessFile(text, true, true).importedFiles.map(f => f.fileName)`
///
/// Lone surrogates produced by escapes inside a specifier are replaced by
/// U+FFFD (see [`imported_files_utf16`] for the exact UTF-16 result).
pub fn imported_files(text: &str) -> Vec<String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    imported_files_utf16(&units).iter().map(|name| String::from_utf16_lossy(name)).collect()
}

/// Same as [`imported_files`], but takes the text as UTF-16 code units and
/// returns each specifier as the exact UTF-16 code units the JS function would
/// have produced.
pub fn imported_files_utf16(text: &[u16]) -> Vec<Vec<u16>> {
    let mut walker = Walker {
        sc: Scanner::new(text),
        imported: Vec::new(),
        ambient: Vec::new(),
        last_token: NO_TOKEN,
        current_token: NO_TOKEN,
        brace_nesting: 0,
        external_module: false,
    };
    walker.process_imports();
    walker.finish()
}

// ---------------------------------------------------------------------------
// Token kinds (the numeric values of `ts.SyntaxKind` in 5.9.3)
// ---------------------------------------------------------------------------

type Kind = u16;

/// Stands for JS `undefined` in `lastToken` / `currentToken`.
const NO_TOKEN: Kind = u16::MAX;

const UNKNOWN: Kind = 0;
const END_OF_FILE: Kind = 1;
const NON_TEXT_FILE_MARKER_TRIVIA: Kind = 8;
const NUMERIC_LITERAL: Kind = 9;
const BIGINT_LITERAL: Kind = 10;
const STRING_LITERAL: Kind = 11;
const NO_SUBSTITUTION_TEMPLATE_LITERAL: Kind = 15;
const TEMPLATE_HEAD: Kind = 16;
const TEMPLATE_MIDDLE: Kind = 17;
const TEMPLATE_TAIL: Kind = 18;
const OPEN_BRACE: Kind = 19;
const CLOSE_BRACE: Kind = 20;
const OPEN_PAREN: Kind = 21;
const CLOSE_PAREN: Kind = 22;
const OPEN_BRACKET: Kind = 23;
const CLOSE_BRACKET: Kind = 24;
const DOT: Kind = 25;
const DOT_DOT_DOT: Kind = 26;
const SEMICOLON: Kind = 27;
const COMMA: Kind = 28;
const QUESTION_DOT: Kind = 29;
const LESS_THAN: Kind = 30;
const GREATER_THAN: Kind = 32;
const LESS_THAN_EQUALS: Kind = 33;
const EQUALS_EQUALS: Kind = 35;
const EXCLAMATION_EQUALS: Kind = 36;
const EQUALS_EQUALS_EQUALS: Kind = 37;
const EXCLAMATION_EQUALS_EQUALS: Kind = 38;
const EQUALS_GREATER_THAN: Kind = 39;
const PLUS: Kind = 40;
const MINUS: Kind = 41;
const ASTERISK: Kind = 42;
const ASTERISK_ASTERISK: Kind = 43;
const SLASH: Kind = 44;
const PERCENT: Kind = 45;
const PLUS_PLUS: Kind = 46;
const MINUS_MINUS: Kind = 47;
const LESS_THAN_LESS_THAN: Kind = 48;
const AMPERSAND: Kind = 51;
const BAR: Kind = 52;
const CARET: Kind = 53;
const EXCLAMATION: Kind = 54;
const TILDE: Kind = 55;
const AMPERSAND_AMPERSAND: Kind = 56;
const BAR_BAR: Kind = 57;
const QUESTION: Kind = 58;
const COLON: Kind = 59;
const AT: Kind = 60;
const QUESTION_QUESTION: Kind = 61;
const EQUALS: Kind = 64;
const PLUS_EQUALS: Kind = 65;
const MINUS_EQUALS: Kind = 66;
const ASTERISK_EQUALS: Kind = 67;
const ASTERISK_ASTERISK_EQUALS: Kind = 68;
const SLASH_EQUALS: Kind = 69;
const PERCENT_EQUALS: Kind = 70;
const LESS_THAN_LESS_THAN_EQUALS: Kind = 71;
const AMPERSAND_EQUALS: Kind = 74;
const BAR_EQUALS: Kind = 75;
const BAR_BAR_EQUALS: Kind = 76;
const AMPERSAND_AMPERSAND_EQUALS: Kind = 77;
const QUESTION_QUESTION_EQUALS: Kind = 78;
const CARET_EQUALS: Kind = 79;
const IDENTIFIER: Kind = 80;
const PRIVATE_IDENTIFIER: Kind = 81;
const FIRST_KEYWORD: Kind = 83;
const IMPORT_KEYWORD: Kind = 102;
const EXPORT_KEYWORD: Kind = 95;
const AS_KEYWORD: Kind = 130;
const DECLARE_KEYWORD: Kind = 138;
const MODULE_KEYWORD: Kind = 144;
const REQUIRE_KEYWORD: Kind = 149;
const TYPE_KEYWORD: Kind = 156;
const FROM_KEYWORD: Kind = 161;
const LAST_KEYWORD: Kind = 166;

fn is_keyword(token: Kind) -> bool {
    (FIRST_KEYWORD..=LAST_KEYWORD).contains(&token)
}

/// `textToKeyword.get(text)` for an ASCII identifier of 2..=12 code units.
fn keyword_kind(text: &[u8]) -> Option<Kind> {
    let kind = match text {
        b"break" => 83,
        b"case" => 84,
        b"catch" => 85,
        b"class" => 86,
        b"const" => 87,
        b"continue" => 88,
        b"debugger" => 89,
        b"default" => 90,
        b"delete" => 91,
        b"do" => 92,
        b"else" => 93,
        b"enum" => 94,
        b"export" => 95,
        b"extends" => 96,
        b"false" => 97,
        b"finally" => 98,
        b"for" => 99,
        b"function" => 100,
        b"if" => 101,
        b"import" => 102,
        b"in" => 103,
        b"instanceof" => 104,
        b"new" => 105,
        b"null" => 106,
        b"return" => 107,
        b"super" => 108,
        b"switch" => 109,
        b"this" => 110,
        b"throw" => 111,
        b"true" => 112,
        b"try" => 113,
        b"typeof" => 114,
        b"var" => 115,
        b"void" => 116,
        b"while" => 117,
        b"with" => 118,
        b"implements" => 119,
        b"interface" => 120,
        b"let" => 121,
        b"package" => 122,
        b"private" => 123,
        b"protected" => 124,
        b"public" => 125,
        b"static" => 126,
        b"yield" => 127,
        b"abstract" => 128,
        b"accessor" => 129,
        b"as" => 130,
        b"asserts" => 131,
        b"assert" => 132,
        b"any" => 133,
        b"async" => 134,
        b"await" => 135,
        b"boolean" => 136,
        b"constructor" => 137,
        b"declare" => 138,
        b"get" => 139,
        b"infer" => 140,
        b"intrinsic" => 141,
        b"is" => 142,
        b"keyof" => 143,
        b"module" => 144,
        b"namespace" => 145,
        b"never" => 146,
        b"out" => 147,
        b"readonly" => 148,
        b"require" => 149,
        b"number" => 150,
        b"object" => 151,
        b"satisfies" => 152,
        b"set" => 153,
        b"string" => 154,
        b"symbol" => 155,
        b"type" => 156,
        b"undefined" => 157,
        b"unique" => 158,
        b"unknown" => 159,
        b"using" => 160,
        b"from" => 161,
        b"global" => 162,
        b"bigint" => 163,
        b"override" => 164,
        b"of" => 165,
        b"defer" => 166,
        _ => return None,
    };
    Some(kind)
}

// ---------------------------------------------------------------------------
// Character classification
// ---------------------------------------------------------------------------

fn is_line_break(ch: i32) -> bool {
    ch == 10 || ch == 13 || ch == 0x2028 || ch == 0x2029
}

fn is_white_space_single_line(ch: i32) -> bool {
    ch == 32
        || ch == 9
        || ch == 11
        || ch == 12
        || ch == 160
        || ch == 133
        || ch == 5760
        || (8192..=8203).contains(&ch)
        || ch == 8239
        || ch == 8287
        || ch == 12288
        || ch == 65279
}

fn is_digit(ch: i32) -> bool {
    (48..=57).contains(&ch)
}

fn is_octal_digit(ch: i32) -> bool {
    (48..=55).contains(&ch)
}

fn is_hex_digit(ch: i32) -> bool {
    is_digit(ch) || (65..=70).contains(&ch) || (97..=102).contains(&ch)
}

fn is_ascii_letter(ch: i32) -> bool {
    (65..=90).contains(&ch) || (97..=122).contains(&ch)
}

fn is_word_character(ch: i32) -> bool {
    is_ascii_letter(ch) || is_digit(ch) || ch == 95
}

/// `isIdentifierStart(ch, ScriptTarget.Latest)`
fn is_identifier_start(ch: i32) -> bool {
    is_ascii_letter(ch) || ch == 36 || ch == 95 || (ch > 127 && lookup_in_unicode_map(ch as u32, &UNICODE_ESNEXT_IDENTIFIER_START))
}

/// `isIdentifierPart(ch, ScriptTarget.Latest)` (standard variant, not JSX)
fn is_identifier_part(ch: i32) -> bool {
    is_word_character(ch) || ch == 36 || (ch > 127 && lookup_in_unicode_map(ch as u32, &UNICODE_ESNEXT_IDENTIFIER_PART))
}

/// `lookupInUnicodeMap`: `map` holds inclusive `[lo, hi]` pairs.
fn lookup_in_unicode_map(code: u32, map: &[u32]) -> bool {
    if code < map[0] {
        return false;
    }
    let mut lo = 0usize;
    let mut hi = map.len();
    while lo + 1 < hi {
        let mut mid = lo + (hi - lo) / 2;
        mid -= mid % 2;
        if map[mid] <= code && code <= map[mid + 1] {
            return true;
        }
        if code < map[mid] {
            hi = mid;
        } else {
            lo = mid + 2;
        }
    }
    false
}

/// `isIdentifierPart` for code points below 128.
static ASCII_IDENTIFIER_PART: [bool; 128] = {
    let mut table = [false; 128];
    let mut i = 0;
    while i < 128 {
        let c = i as u8;
        table[i] = c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
        i += 1;
    }
    table
};

/// `charSize`: UTF-16 length of a code point (0 for EOF).
fn char_size(ch: i32) -> usize {
    if ch >= 65536 {
        2
    } else if ch == -1 {
        0
    } else {
        1
    }
}

/// `utf16EncodeAsString` (`String.fromCodePoint`); lone surrogate code points
/// become a single code unit.
fn push_code_point(out: &mut Vec<u16>, code_point: u32) {
    if code_point >= 0x10000 {
        let v = code_point - 0x10000;
        out.push(0xD800 + (v >> 10) as u16);
        out.push(0xDC00 + (v & 0x3FF) as u16);
    } else {
        out.push(code_point as u16);
    }
}

/// Caps a hex value so that it can never overflow; every value above
/// 0x10FFFF is treated the same by the scanner (invalid / not an identifier).
fn push_hex_digit(value: i32, digit: i32) -> i32 {
    (value * 16 + digit).min(0x100_0000)
}

fn hex_value(ch: i32) -> i32 {
    if ch >= 97 {
        ch - 97 + 10
    } else if ch >= 65 {
        ch - 65 + 10
    } else {
        ch - 48
    }
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// The state of `createScanner(Latest, skipTrivia = true)` that the walker can
/// observe. Error reporting, JSX, JSDoc, regex and comment-directive handling
/// are not ported: none of them changes `pos`, `token` or `tokenValue`.
struct Scanner<'a> {
    text: &'a [u16],
    end: usize,
    pos: usize,
    token_start: usize,
    token: Kind,
    /// `tokenValue`: `text[value_start..value_end]`, or `owned` when
    /// `value_owned` (cooked values that differ from the raw text).
    value_start: usize,
    value_end: usize,
    value_owned: bool,
    owned: Vec<u16>,
}

impl<'a> Scanner<'a> {
    /// `setText(text)`
    fn new(text: &'a [u16]) -> Self {
        Scanner {
            text,
            end: text.len(),
            pos: 0,
            token_start: 0,
            token: UNKNOWN,
            value_start: 0,
            value_end: 0,
            value_owned: false,
            owned: Vec::new(),
        }
    }

    /// `getTokenValue()`
    fn token_value(&self) -> &[u16] {
        if self.value_owned { &self.owned } else { &self.text[self.value_start..self.value_end] }
    }

    fn set_value_range(&mut self, start: usize, end: usize) {
        self.value_start = start;
        self.value_end = end;
        self.value_owned = false;
    }

    /// Hands out the (cleared) buffer for a cooked token value; give it back
    /// with `put_owned`.
    fn take_owned(&mut self) -> Vec<u16> {
        let mut buffer = std::mem::take(&mut self.owned);
        buffer.clear();
        buffer
    }

    fn put_owned(&mut self, buffer: Vec<u16>) {
        self.owned = buffer;
        self.value_owned = true;
    }

    /// `charCodeUnchecked`: -1 stands for JS `NaN` (past the end).
    #[inline]
    fn unit(&self, index: usize) -> i32 {
        match self.text.get(index) {
            Some(&u) => i32::from(u),
            None => -1,
        }
    }

    /// `codePointUnchecked`: -1 stands for JS `undefined` (past the end).
    #[inline]
    fn code_point(&self, index: usize) -> i32 {
        match self.text.get(index) {
            None => -1,
            Some(&hi) if (0xD800..0xDC00).contains(&hi) => match self.text.get(index + 1) {
                Some(&lo) if (0xDC00..0xE000).contains(&lo) => 0x10000 + ((i32::from(hi) - 0xD800) << 10) + (i32::from(lo) - 0xDC00),
                _ => i32::from(hi),
            },
            Some(&u) => i32::from(u),
        }
    }

    #[inline]
    fn finish_token(&mut self, len: usize, kind: Kind) -> Kind {
        self.pos += len;
        self.token = kind;
        kind
    }

    /// `lookAhead(callback)`: always restores the scanner state.
    fn look_ahead<R>(&mut self, callback: impl FnOnce(&mut Self) -> R) -> R {
        let save_pos = self.pos;
        let save_token_start = self.token_start;
        let save_token = self.token;
        let save_value = (self.value_start, self.value_end, self.value_owned);
        let save_owned = if self.value_owned { Some(self.owned.clone()) } else { None };
        let result = callback(self);
        self.pos = save_pos;
        self.token_start = save_token_start;
        self.token = save_token;
        (self.value_start, self.value_end, self.value_owned) = save_value;
        if let Some(owned) = save_owned {
            self.owned = owned;
        }
        result
    }

    /// `reScanTemplateToken(isTaggedTemplate)`
    fn re_scan_template_token(&mut self, is_tagged_template: bool) -> Kind {
        self.pos = self.token_start;
        self.token = self.scan_template_and_set_token_value(!is_tagged_template);
        self.token
    }

    // -- numbers ------------------------------------------------------------

    /// `scanNumberFragment` (only the end position is observable).
    fn scan_number_fragment(&mut self) {
        loop {
            let ch = self.unit(self.pos);
            if ch == 95 || is_digit(ch) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// `scanNumber` (only the end position and the literal kind are observable).
    fn scan_number(&mut self) -> Kind {
        let mut contains_leading_zero = false;
        if self.unit(self.pos) == 48 {
            self.pos += 1;
            if self.unit(self.pos) == 95 {
                self.pos -= 1;
                self.scan_number_fragment();
            } else {
                // scanDigits()
                let digits_start = self.pos;
                let mut is_octal = true;
                while is_digit(self.unit(self.pos)) {
                    if !is_octal_digit(self.unit(self.pos)) {
                        is_octal = false;
                    }
                    self.pos += 1;
                }
                if !is_octal {
                    contains_leading_zero = true;
                } else if self.pos != digits_start {
                    // legacy octal literal such as 0777
                    return NUMERIC_LITERAL;
                }
            }
        } else {
            self.scan_number_fragment();
        }
        let mut has_decimal = false;
        let mut scientific = false;
        if self.unit(self.pos) == 46 {
            self.pos += 1;
            has_decimal = true;
            self.scan_number_fragment();
        }
        if self.unit(self.pos) == 69 || self.unit(self.pos) == 101 {
            self.pos += 1;
            scientific = true;
            if self.unit(self.pos) == 43 || self.unit(self.pos) == 45 {
                self.pos += 1;
            }
            self.scan_number_fragment();
        }
        if contains_leading_zero {
            return NUMERIC_LITERAL;
        }
        if has_decimal || scientific {
            self.check_for_identifier_start_after_numeric_literal();
            NUMERIC_LITERAL
        } else {
            let kind = self.check_big_int_suffix();
            self.check_for_identifier_start_after_numeric_literal();
            kind
        }
    }

    fn check_for_identifier_start_after_numeric_literal(&mut self) {
        if !is_identifier_start(self.code_point(self.pos)) {
            return;
        }
        let identifier_start = self.pos;
        let mut parts = Vec::new();
        self.scan_identifier_parts(&mut parts);
        // `1.5n` / `1e3n`: the `n` stays consumed; anything else is given back.
        if !(parts.len() == 1 && self.unit(identifier_start) == 110) {
            self.pos = identifier_start;
        }
    }

    fn check_big_int_suffix(&mut self) -> Kind {
        if self.unit(self.pos) == 110 {
            self.pos += 1;
            BIGINT_LITERAL
        } else {
            NUMERIC_LITERAL
        }
    }

    /// `scanMinimumNumberOfHexDigits(1, canHaveSeparators)` (end position only).
    fn skip_hex_digits_and_separators(&mut self) {
        loop {
            let ch = self.unit(self.pos);
            if ch == 95 || is_hex_digit(ch) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// `scanBinaryOrOctalDigits(base)` (end position only).
    fn skip_binary_or_octal_digits(&mut self, base: i32) {
        loop {
            let ch = self.unit(self.pos);
            if ch == 95 {
                self.pos += 1;
                continue;
            }
            if !is_digit(ch) || ch - 48 >= base {
                break;
            }
            self.pos += 1;
        }
    }

    // -- strings, templates, escapes ----------------------------------------

    /// `scanString`; sets the cooked value as the token value.
    fn scan_string(&mut self) {
        let text = self.text;
        let quote = self.unit(self.pos);
        self.pos += 1;
        let first = self.pos;
        // Fast path: no escape sequence, so the value is a slice of the text.
        loop {
            if self.pos >= self.end {
                self.set_value_range(first, self.pos);
                return;
            }
            let ch = self.unit(self.pos);
            if ch == quote {
                self.set_value_range(first, self.pos);
                self.pos += 1;
                return;
            }
            if ch == 92 {
                break;
            }
            if ch == 10 || ch == 13 {
                self.set_value_range(first, self.pos);
                return;
            }
            self.pos += 1;
        }
        let mut out = self.take_owned();
        let mut start = first;
        loop {
            if self.pos >= self.end {
                out.extend_from_slice(&text[start..self.pos]);
                break;
            }
            let ch = self.unit(self.pos);
            if ch == quote {
                out.extend_from_slice(&text[start..self.pos]);
                self.pos += 1;
                break;
            }
            if ch == 92 {
                out.extend_from_slice(&text[start..self.pos]);
                self.scan_escape_sequence(true, &mut out);
                start = self.pos;
                continue;
            }
            if ch == 10 || ch == 13 {
                out.extend_from_slice(&text[start..self.pos]);
                break;
            }
            self.pos += 1;
        }
        self.put_owned(out);
    }

    /// `scanTemplateAndSetTokenValue(shouldEmitInvalidEscapeError)`
    fn scan_template_and_set_token_value(&mut self, should_emit_invalid_escape_error: bool) -> Kind {
        let text = self.text;
        let mut contents = self.take_owned();
        let started_with_backtick = self.unit(self.pos) == 96;
        self.pos += 1;
        let mut start = self.pos;
        let resulting_token;
        loop {
            if self.pos >= self.end {
                contents.extend_from_slice(&text[start..self.pos]);
                resulting_token = if started_with_backtick { NO_SUBSTITUTION_TEMPLATE_LITERAL } else { TEMPLATE_TAIL };
                break;
            }
            let curr_char = self.unit(self.pos);
            if curr_char == 96 {
                contents.extend_from_slice(&text[start..self.pos]);
                self.pos += 1;
                resulting_token = if started_with_backtick { NO_SUBSTITUTION_TEMPLATE_LITERAL } else { TEMPLATE_TAIL };
                break;
            }
            if curr_char == 36 && self.pos + 1 < self.end && self.unit(self.pos + 1) == 123 {
                contents.extend_from_slice(&text[start..self.pos]);
                self.pos += 2;
                resulting_token = if started_with_backtick { TEMPLATE_HEAD } else { TEMPLATE_MIDDLE };
                break;
            }
            if curr_char == 92 {
                contents.extend_from_slice(&text[start..self.pos]);
                self.scan_escape_sequence(should_emit_invalid_escape_error, &mut contents);
                start = self.pos;
                continue;
            }
            if curr_char == 13 {
                contents.extend_from_slice(&text[start..self.pos]);
                self.pos += 1;
                if self.pos < self.end && self.unit(self.pos) == 10 {
                    self.pos += 1;
                }
                contents.push(10);
                start = self.pos;
                continue;
            }
            self.pos += 1;
        }
        self.put_owned(contents);
        resulting_token
    }

    /// `scanEscapeSequence(String | (report ? ReportErrors : 0))`; appends the
    /// cooked text to `out`. `report` matters because an octal or `\8` / `\9`
    /// escape is cooked when errors are reported and left raw otherwise.
    fn scan_escape_sequence(&mut self, report: bool, out: &mut Vec<u16>) {
        let text = self.text;
        let start = self.pos;
        self.pos += 1;
        if self.pos >= self.end {
            return;
        }
        let ch = self.unit(self.pos);
        self.pos += 1;
        match ch {
            48..=55 => {
                if ch == 48 && (self.pos >= self.end || !is_digit(self.unit(self.pos))) {
                    out.push(0);
                    return;
                }
                if ch <= 51 && self.pos < self.end && is_octal_digit(self.unit(self.pos)) {
                    self.pos += 1;
                }
                if self.pos < self.end && is_octal_digit(self.unit(self.pos)) {
                    self.pos += 1;
                }
                if report {
                    let mut code: u16 = 0;
                    for &digit in &text[start + 1..self.pos] {
                        code = code * 8 + (digit - 48);
                    }
                    out.push(code);
                } else {
                    out.extend_from_slice(&text[start..self.pos]);
                }
            }
            56 | 57 => {
                if report {
                    out.push(ch as u16);
                } else {
                    out.extend_from_slice(&text[start..self.pos]);
                }
            }
            98 => out.push(8),
            116 => out.push(9),
            110 => out.push(10),
            118 => out.push(11),
            102 => out.push(12),
            114 => out.push(13),
            39 => out.push(39),
            34 => out.push(34),
            117 => {
                if self.pos < self.end && self.unit(self.pos) == 123 {
                    self.pos -= 2;
                    self.scan_extended_unicode_escape(out);
                    return;
                }
                while self.pos < start + 6 {
                    if !(self.pos < self.end && is_hex_digit(self.unit(self.pos))) {
                        out.extend_from_slice(&text[start..self.pos]);
                        return;
                    }
                    self.pos += 1;
                }
                let mut value = 0;
                for &digit in &text[start + 2..self.pos] {
                    value = value * 16 + hex_value(i32::from(digit));
                }
                out.push(value as u16);
            }
            120 => {
                while self.pos < start + 4 {
                    if !(self.pos < self.end && is_hex_digit(self.unit(self.pos))) {
                        out.extend_from_slice(&text[start..self.pos]);
                        return;
                    }
                    self.pos += 1;
                }
                let mut value = 0;
                for &digit in &text[start + 2..self.pos] {
                    value = value * 16 + hex_value(i32::from(digit));
                }
                out.push(value as u16);
            }
            13 => {
                if self.pos < self.end && self.unit(self.pos) == 10 {
                    self.pos += 1;
                }
            }
            10 | 0x2028 | 0x2029 => {}
            _ => out.push(ch as u16),
        }
    }

    /// `scanMinimumNumberOfHexDigits(1, false)` followed by `parseInt(_, 16)`:
    /// -1 when there is no digit, otherwise the value capped above 0x10FFFF.
    fn scan_hex_digits_value(&mut self) -> i32 {
        let mut value = -1;
        loop {
            let ch = self.unit(self.pos);
            if !is_hex_digit(ch) {
                break;
            }
            value = push_hex_digit(value.max(0), hex_value(ch));
            self.pos += 1;
        }
        value
    }

    /// `scanExtendedUnicodeEscape(_)`; the cursor is on the backslash.
    fn scan_extended_unicode_escape(&mut self, out: &mut Vec<u16>) {
        let text = self.text;
        let start = self.pos;
        self.pos += 3;
        let value = self.scan_hex_digits_value();
        let mut invalid = !(0..=1_114_111).contains(&value);
        if self.pos >= self.end {
            invalid = true;
        } else if self.unit(self.pos) == 125 {
            self.pos += 1;
        } else {
            invalid = true;
        }
        if invalid {
            out.extend_from_slice(&text[start..self.pos]);
        } else {
            push_code_point(out, value as u32);
        }
    }

    /// `peekUnicodeEscape`: value of `\uXXXX` at the cursor, or -1.
    fn peek_unicode_escape(&self) -> i32 {
        if self.pos + 5 < self.end && self.unit(self.pos + 1) == 117 {
            let mut value = 0;
            for offset in 2..6 {
                let ch = self.unit(self.pos + offset);
                if !is_hex_digit(ch) {
                    return -1;
                }
                value = value * 16 + hex_value(ch);
            }
            return value;
        }
        -1
    }

    /// `peekExtendedUnicodeEscape`: value of `\u{X...}` at the cursor (the
    /// closing brace is not checked), or -1.
    fn peek_extended_unicode_escape(&mut self) -> i32 {
        if self.code_point(self.pos + 1) == 117 && self.code_point(self.pos + 2) == 123 {
            let start = self.pos;
            self.pos += 3;
            let value = self.scan_hex_digits_value();
            self.pos = start;
            return value;
        }
        -1
    }

    // -- identifiers --------------------------------------------------------

    /// `scanIdentifierParts`; appends the cooked parts to `result`.
    fn scan_identifier_parts(&mut self, result: &mut Vec<u16>) {
        let text = self.text;
        let mut start = self.pos;
        while self.pos < self.end {
            let mut ch = self.code_point(self.pos);
            if is_identifier_part(ch) {
                self.pos += char_size(ch);
            } else if ch == 92 {
                ch = self.peek_extended_unicode_escape();
                if ch >= 0 && is_identifier_part(ch) {
                    // TS does not append text[start..pos] here; kept as is.
                    self.scan_extended_unicode_escape(result);
                    start = self.pos;
                    continue;
                }
                ch = self.peek_unicode_escape();
                if !(ch >= 0 && is_identifier_part(ch)) {
                    break;
                }
                result.extend_from_slice(&text[start..self.pos]);
                push_code_point(result, ch as u32);
                self.pos += 6;
                start = self.pos;
            } else {
                break;
            }
        }
        result.extend_from_slice(&text[start..self.pos]);
    }

    /// `getIdentifierToken`
    fn get_identifier_token(&mut self) -> Kind {
        let value = self.token_value();
        let len = value.len();
        if (2..=12).contains(&len) && (97..=122).contains(&value[0]) {
            let mut ascii = [0u8; 12];
            let mut all_ascii = true;
            for (slot, &unit) in ascii.iter_mut().zip(value.iter()) {
                if unit > 127 {
                    all_ascii = false;
                    break;
                }
                *slot = unit as u8;
            }
            if all_ascii && let Some(keyword) = keyword_kind(&ascii[..len]) {
                self.token = keyword;
                return keyword;
            }
        }
        self.token = IDENTIFIER;
        IDENTIFIER
    }

    /// `scanIdentifier(startCharacter, Latest)`; the cursor is on the first
    /// character (or just after the `#` of a private name).
    fn scan_identifier(&mut self, start_character: i32) -> Option<Kind> {
        let mut ch = start_character;
        if !is_identifier_start(ch) {
            return None;
        }
        self.pos += char_size(ch);
        let text = self.text;
        while self.pos < self.end {
            let unit = text[self.pos];
            if unit < 128 {
                // ASCII fast path (same answer as `isIdentifierPart`)
                if ASCII_IDENTIFIER_PART[usize::from(unit)] {
                    self.pos += 1;
                    continue;
                }
                ch = i32::from(unit);
                break;
            }
            ch = self.code_point(self.pos);
            if !is_identifier_part(ch) {
                break;
            }
            self.pos += char_size(ch);
        }
        if self.pos < self.end && ch == 92 {
            let mut value = self.take_owned();
            value.extend_from_slice(&text[self.token_start..self.pos]);
            self.scan_identifier_parts(&mut value);
            self.put_owned(value);
        } else {
            self.set_value_range(self.token_start, self.pos);
        }
        Some(self.get_identifier_token())
    }

    // -- trivia -------------------------------------------------------------

    /// `isConflictMarkerTrivia(text, pos)`
    fn is_conflict_marker_trivia(&self, pos: usize) -> bool {
        const LEN: usize = 7;
        if pos == 0 || is_line_break(self.unit(pos - 1)) {
            let ch = self.unit(pos);
            if pos + LEN < self.text.len() {
                for i in 0..LEN {
                    if self.unit(pos + i) != ch {
                        return false;
                    }
                }
                return ch == 61 || self.unit(pos + LEN) == 32;
            }
        }
        false
    }

    /// `scanConflictMarkerTrivia(text, pos)`
    fn scan_conflict_marker_trivia(&self, mut pos: usize) -> usize {
        let ch = self.unit(pos);
        let len = self.text.len();
        if ch == 60 || ch == 62 {
            while pos < len && !is_line_break(self.unit(pos)) {
                pos += 1;
            }
        } else {
            while pos < len {
                let current = self.unit(pos);
                if (current == 61 || current == 62) && current != ch && self.is_conflict_marker_trivia(pos) {
                    break;
                }
                pos += 1;
            }
        }
        pos
    }

    /// The `isConflictMarkerTrivia` / `scanConflictMarkerTrivia` step of `scan`.
    /// Returns true when a marker was skipped.
    fn skip_conflict_marker(&mut self) -> bool {
        if self.is_conflict_marker_trivia(self.pos) {
            self.pos = self.scan_conflict_marker_trivia(self.pos);
            true
        } else {
            false
        }
    }

    // -- scan ---------------------------------------------------------------

    /// `scan()` with `skipTrivia = true`
    fn scan(&mut self) -> Kind {
        loop {
            self.token_start = self.pos;
            if self.pos >= self.end {
                self.token = END_OF_FILE;
                return END_OF_FILE;
            }
            let ch = self.code_point(self.pos);
            if self.pos == 0 && ch == 35 && self.unit(1) == 33 {
                // shebang: /^#!.*/
                self.pos = 2;
                while self.pos < self.end && !is_line_break(self.unit(self.pos)) {
                    self.pos += 1;
                }
                continue;
            }
            match ch {
                10 | 13 | 9 | 11 | 12 | 32 => {
                    // run of ASCII whitespace / line breaks (all skipped as trivia)
                    self.pos += 1;
                    while self.pos < self.end && matches!(self.text[self.pos], 9..=13 | 32) {
                        self.pos += 1;
                    }
                    continue;
                }
                160 | 5760 | 8192..=8203 | 8239 | 8287 | 12288 | 65279 => {
                    self.pos += 1;
                    continue;
                }
                33 => {
                    if self.unit(self.pos + 1) == 61 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, EXCLAMATION_EQUALS_EQUALS);
                        }
                        return self.finish_token(2, EXCLAMATION_EQUALS);
                    }
                    return self.finish_token(1, EXCLAMATION);
                }
                34 | 39 => {
                    self.scan_string();
                    self.token = STRING_LITERAL;
                    return STRING_LITERAL;
                }
                96 => {
                    self.token = self.scan_template_and_set_token_value(false);
                    return self.token;
                }
                37 => {
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, PERCENT_EQUALS);
                    }
                    return self.finish_token(1, PERCENT);
                }
                38 => {
                    if self.unit(self.pos + 1) == 38 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, AMPERSAND_AMPERSAND_EQUALS);
                        }
                        return self.finish_token(2, AMPERSAND_AMPERSAND);
                    }
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, AMPERSAND_EQUALS);
                    }
                    return self.finish_token(1, AMPERSAND);
                }
                40 => return self.finish_token(1, OPEN_PAREN),
                41 => return self.finish_token(1, CLOSE_PAREN),
                42 => {
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, ASTERISK_EQUALS);
                    }
                    if self.unit(self.pos + 1) == 42 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, ASTERISK_ASTERISK_EQUALS);
                        }
                        return self.finish_token(2, ASTERISK_ASTERISK);
                    }
                    return self.finish_token(1, ASTERISK);
                }
                43 => {
                    if self.unit(self.pos + 1) == 43 {
                        return self.finish_token(2, PLUS_PLUS);
                    }
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, PLUS_EQUALS);
                    }
                    return self.finish_token(1, PLUS);
                }
                44 => return self.finish_token(1, COMMA),
                45 => {
                    if self.unit(self.pos + 1) == 45 {
                        return self.finish_token(2, MINUS_MINUS);
                    }
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, MINUS_EQUALS);
                    }
                    return self.finish_token(1, MINUS);
                }
                46 => {
                    if is_digit(self.unit(self.pos + 1)) {
                        self.scan_number();
                        self.token = NUMERIC_LITERAL;
                        return NUMERIC_LITERAL;
                    }
                    if self.unit(self.pos + 1) == 46 && self.unit(self.pos + 2) == 46 {
                        return self.finish_token(3, DOT_DOT_DOT);
                    }
                    return self.finish_token(1, DOT);
                }
                47 => {
                    if self.unit(self.pos + 1) == 47 {
                        self.pos += 2;
                        let rest = &self.text[self.pos..self.end];
                        self.pos += rest.iter().position(|&u| matches!(u, 10 | 13 | 0x2028 | 0x2029)).unwrap_or(rest.len());
                        continue;
                    }
                    if self.unit(self.pos + 1) == 42 {
                        self.pos += 2;
                        // find `*/`; same result as stepping one unit at a time
                        loop {
                            let rest = &self.text[self.pos..self.end];
                            match rest.iter().position(|&u| u == 42) {
                                None => {
                                    self.pos = self.end;
                                    break;
                                }
                                Some(offset) => {
                                    self.pos += offset;
                                    if self.unit(self.pos + 1) == 47 {
                                        self.pos += 2;
                                        break;
                                    }
                                    self.pos += 1;
                                }
                            }
                        }
                        continue;
                    }
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, SLASH_EQUALS);
                    }
                    return self.finish_token(1, SLASH);
                }
                48 => {
                    if self.pos + 2 < self.end && (self.unit(self.pos + 1) == 88 || self.unit(self.pos + 1) == 120) {
                        self.pos += 2;
                        self.skip_hex_digits_and_separators();
                        self.token = self.check_big_int_suffix();
                        return self.token;
                    } else if self.pos + 2 < self.end && (self.unit(self.pos + 1) == 66 || self.unit(self.pos + 1) == 98) {
                        self.pos += 2;
                        self.skip_binary_or_octal_digits(2);
                        self.token = self.check_big_int_suffix();
                        return self.token;
                    } else if self.pos + 2 < self.end && (self.unit(self.pos + 1) == 79 || self.unit(self.pos + 1) == 111) {
                        self.pos += 2;
                        self.skip_binary_or_octal_digits(8);
                        self.token = self.check_big_int_suffix();
                        return self.token;
                    }
                    self.token = self.scan_number();
                    return self.token;
                }
                49..=57 => {
                    self.token = self.scan_number();
                    return self.token;
                }
                58 => return self.finish_token(1, COLON),
                59 => return self.finish_token(1, SEMICOLON),
                60 => {
                    if self.skip_conflict_marker() {
                        continue;
                    }
                    if self.unit(self.pos + 1) == 60 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, LESS_THAN_LESS_THAN_EQUALS);
                        }
                        return self.finish_token(2, LESS_THAN_LESS_THAN);
                    }
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, LESS_THAN_EQUALS);
                    }
                    return self.finish_token(1, LESS_THAN);
                }
                61 => {
                    if self.skip_conflict_marker() {
                        continue;
                    }
                    if self.unit(self.pos + 1) == 61 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, EQUALS_EQUALS_EQUALS);
                        }
                        return self.finish_token(2, EQUALS_EQUALS);
                    }
                    if self.unit(self.pos + 1) == 62 {
                        return self.finish_token(2, EQUALS_GREATER_THAN);
                    }
                    return self.finish_token(1, EQUALS);
                }
                62 => {
                    if self.skip_conflict_marker() {
                        continue;
                    }
                    return self.finish_token(1, GREATER_THAN);
                }
                63 => {
                    if self.unit(self.pos + 1) == 46 && !is_digit(self.unit(self.pos + 2)) {
                        return self.finish_token(2, QUESTION_DOT);
                    }
                    if self.unit(self.pos + 1) == 63 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, QUESTION_QUESTION_EQUALS);
                        }
                        return self.finish_token(2, QUESTION_QUESTION);
                    }
                    return self.finish_token(1, QUESTION);
                }
                91 => return self.finish_token(1, OPEN_BRACKET),
                93 => return self.finish_token(1, CLOSE_BRACKET),
                94 => {
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, CARET_EQUALS);
                    }
                    return self.finish_token(1, CARET);
                }
                123 => return self.finish_token(1, OPEN_BRACE),
                124 => {
                    if self.skip_conflict_marker() {
                        continue;
                    }
                    if self.unit(self.pos + 1) == 124 {
                        if self.unit(self.pos + 2) == 61 {
                            return self.finish_token(3, BAR_BAR_EQUALS);
                        }
                        return self.finish_token(2, BAR_BAR);
                    }
                    if self.unit(self.pos + 1) == 61 {
                        return self.finish_token(2, BAR_EQUALS);
                    }
                    return self.finish_token(1, BAR);
                }
                125 => return self.finish_token(1, CLOSE_BRACE),
                126 => return self.finish_token(1, TILDE),
                64 => return self.finish_token(1, AT),
                92 => {
                    let extended = self.peek_extended_unicode_escape();
                    if extended >= 0 && is_identifier_start(extended) {
                        let mut value = self.take_owned();
                        self.scan_extended_unicode_escape(&mut value);
                        self.scan_identifier_parts(&mut value);
                        self.put_owned(value);
                        return self.get_identifier_token();
                    }
                    let cooked = self.peek_unicode_escape();
                    if cooked >= 0 && is_identifier_start(cooked) {
                        self.pos += 6;
                        let mut value = self.take_owned();
                        value.push(cooked as u16);
                        self.scan_identifier_parts(&mut value);
                        self.put_owned(value);
                        return self.get_identifier_token();
                    }
                    return self.finish_token(1, UNKNOWN);
                }
                35 => {
                    if self.pos != 0 && self.unit(self.pos + 1) == 33 {
                        return self.finish_token(1, UNKNOWN);
                    }
                    let char_after_hash = self.code_point(self.pos + 1);
                    if char_after_hash == 92 {
                        self.pos += 1;
                        let extended = self.peek_extended_unicode_escape();
                        if extended >= 0 && is_identifier_start(extended) {
                            let mut value = self.take_owned();
                            value.push(35);
                            self.scan_extended_unicode_escape(&mut value);
                            self.scan_identifier_parts(&mut value);
                            self.put_owned(value);
                            self.token = PRIVATE_IDENTIFIER;
                            return PRIVATE_IDENTIFIER;
                        }
                        let cooked = self.peek_unicode_escape();
                        if cooked >= 0 && is_identifier_start(cooked) {
                            self.pos += 6;
                            let mut value = self.take_owned();
                            value.push(35);
                            value.push(cooked as u16);
                            self.scan_identifier_parts(&mut value);
                            self.put_owned(value);
                            self.token = PRIVATE_IDENTIFIER;
                            return PRIVATE_IDENTIFIER;
                        }
                        self.pos -= 1;
                    }
                    if is_identifier_start(char_after_hash) {
                        self.pos += 1;
                        self.scan_identifier(char_after_hash);
                    } else {
                        let mut value = self.take_owned();
                        value.push(35);
                        self.put_owned(value);
                        self.pos += 1;
                    }
                    self.token = PRIVATE_IDENTIFIER;
                    return PRIVATE_IDENTIFIER;
                }
                65533 => {
                    self.pos = self.end;
                    self.token = NON_TEXT_FILE_MARKER_TRIVIA;
                    return NON_TEXT_FILE_MARKER_TRIVIA;
                }
                _ => {
                    if let Some(kind) = self.scan_identifier(ch) {
                        return kind;
                    } else if is_white_space_single_line(ch) || is_line_break(ch) {
                        self.pos += char_size(ch);
                        continue;
                    }
                    return self.finish_token(char_size(ch), UNKNOWN);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// preProcessFile
// ---------------------------------------------------------------------------

/// State of one `preProcessFile` call.
struct Walker<'a> {
    sc: Scanner<'a>,
    imported: Vec<Vec<u16>>,
    /// `ambientExternalModules`: (specifier, brace depth at the string literal)
    ambient: Vec<(Vec<u16>, i64)>,
    last_token: Kind,
    current_token: Kind,
    brace_nesting: i64,
    external_module: bool,
}

impl Walker<'_> {
    fn next_token(&mut self) -> Kind {
        self.last_token = self.current_token;
        self.current_token = self.sc.scan();
        if self.current_token == OPEN_BRACE {
            self.brace_nesting += 1;
        } else if self.current_token == CLOSE_BRACE {
            self.brace_nesting -= 1;
        }
        self.current_token
    }

    fn record_ambient_external_module(&mut self) {
        self.ambient.push((self.sc.token_value().to_vec(), self.brace_nesting));
    }

    fn record_module_name(&mut self) {
        self.imported.push(self.sc.token_value().to_vec());
        self.mark_as_external_module_if_top_level();
    }

    fn mark_as_external_module_if_top_level(&mut self) {
        if self.brace_nesting == 0 {
            self.external_module = true;
        }
    }

    fn try_consume_declare(&mut self) -> bool {
        let mut token = self.sc.token;
        if token == DECLARE_KEYWORD {
            token = self.next_token();
            if token == MODULE_KEYWORD {
                token = self.next_token();
                if token == STRING_LITERAL {
                    self.record_ambient_external_module();
                }
            }
            return true;
        }
        false
    }

    fn try_consume_import(&mut self) -> bool {
        if self.last_token == DOT {
            return false;
        }
        let mut token = self.sc.token;
        if token == IMPORT_KEYWORD {
            token = self.next_token();
            if token == OPEN_PAREN {
                token = self.next_token();
                if token == STRING_LITERAL || token == NO_SUBSTITUTION_TEMPLATE_LITERAL {
                    self.record_module_name();
                    return true;
                }
            } else if token == STRING_LITERAL {
                self.record_module_name();
                return true;
            } else {
                if token == TYPE_KEYWORD {
                    let skip_type_keyword = self.sc.look_ahead(|sc| {
                        let token2 = sc.scan();
                        token2 != FROM_KEYWORD && (token2 == ASTERISK || token2 == OPEN_BRACE || token2 == IDENTIFIER || is_keyword(token2))
                    });
                    if skip_type_keyword {
                        token = self.next_token();
                    }
                }
                if token == IDENTIFIER || is_keyword(token) {
                    token = self.next_token();
                    if token == FROM_KEYWORD {
                        token = self.next_token();
                        if token == STRING_LITERAL {
                            self.record_module_name();
                            return true;
                        }
                    } else if token == EQUALS {
                        if self.try_consume_require_call(true, false) {
                            return true;
                        }
                    } else if token == COMMA {
                        token = self.next_token();
                    } else {
                        return true;
                    }
                }
                if token == OPEN_BRACE {
                    token = self.next_token();
                    while token != CLOSE_BRACE && token != END_OF_FILE {
                        token = self.next_token();
                    }
                    if token == CLOSE_BRACE {
                        token = self.next_token();
                        if token == FROM_KEYWORD {
                            token = self.next_token();
                            if token == STRING_LITERAL {
                                self.record_module_name();
                            }
                        }
                    }
                } else if token == ASTERISK {
                    token = self.next_token();
                    if token == AS_KEYWORD {
                        token = self.next_token();
                        if token == IDENTIFIER || is_keyword(token) {
                            token = self.next_token();
                            if token == FROM_KEYWORD {
                                token = self.next_token();
                                if token == STRING_LITERAL {
                                    self.record_module_name();
                                }
                            }
                        }
                    }
                }
            }
            return true;
        }
        false
    }

    fn try_consume_export(&mut self) -> bool {
        let mut token = self.sc.token;
        if token == EXPORT_KEYWORD {
            self.mark_as_external_module_if_top_level();
            token = self.next_token();
            if token == TYPE_KEYWORD {
                let skip_type_keyword = self.sc.look_ahead(|sc| {
                    let token2 = sc.scan();
                    token2 == ASTERISK || token2 == OPEN_BRACE
                });
                if skip_type_keyword {
                    token = self.next_token();
                }
            }
            if token == OPEN_BRACE {
                token = self.next_token();
                while token != CLOSE_BRACE && token != END_OF_FILE {
                    token = self.next_token();
                }
                if token == CLOSE_BRACE {
                    token = self.next_token();
                    if token == FROM_KEYWORD {
                        token = self.next_token();
                        if token == STRING_LITERAL {
                            self.record_module_name();
                        }
                    }
                }
            } else if token == ASTERISK {
                token = self.next_token();
                if token == FROM_KEYWORD {
                    token = self.next_token();
                    if token == STRING_LITERAL {
                        self.record_module_name();
                    }
                }
            } else if token == IMPORT_KEYWORD {
                token = self.next_token();
                if token == TYPE_KEYWORD {
                    let skip_type_keyword = self.sc.look_ahead(|sc| {
                        let token2 = sc.scan();
                        token2 == IDENTIFIER || is_keyword(token2)
                    });
                    if skip_type_keyword {
                        token = self.next_token();
                    }
                }
                if token == IDENTIFIER || is_keyword(token) {
                    token = self.next_token();
                    if token == EQUALS && self.try_consume_require_call(true, false) {
                        return true;
                    }
                }
            }
            return true;
        }
        false
    }

    fn try_consume_require_call(&mut self, skip_current_token: bool, allow_template_literals: bool) -> bool {
        let mut token = if skip_current_token { self.next_token() } else { self.sc.token };
        if token == REQUIRE_KEYWORD {
            token = self.next_token();
            if token == OPEN_PAREN {
                token = self.next_token();
                if token == STRING_LITERAL || (allow_template_literals && token == NO_SUBSTITUTION_TEMPLATE_LITERAL) {
                    self.record_module_name();
                }
            }
            return true;
        }
        false
    }

    fn try_consume_define(&mut self) -> bool {
        let mut token = self.sc.token;
        if token == IDENTIFIER && is_ascii_text(self.sc.token_value(), b"define") {
            token = self.next_token();
            if token != OPEN_PAREN {
                return true;
            }
            token = self.next_token();
            if token == STRING_LITERAL || token == NO_SUBSTITUTION_TEMPLATE_LITERAL {
                token = self.next_token();
                if token == COMMA {
                    token = self.next_token();
                } else {
                    return true;
                }
            }
            if token != OPEN_BRACKET {
                return true;
            }
            token = self.next_token();
            while token != CLOSE_BRACKET && token != END_OF_FILE {
                if token == STRING_LITERAL || token == NO_SUBSTITUTION_TEMPLATE_LITERAL {
                    self.record_module_name();
                }
                token = self.next_token();
            }
            return true;
        }
        false
    }

    fn process_imports(&mut self) {
        self.next_token();
        loop {
            if self.sc.token == END_OF_FILE {
                break;
            }
            if self.sc.token == TEMPLATE_HEAD {
                let mut stack: Vec<Kind> = vec![self.sc.token];
                while !stack.is_empty() {
                    let token = self.sc.scan();
                    match token {
                        END_OF_FILE => break,
                        IMPORT_KEYWORD => {
                            self.try_consume_import();
                        }
                        TEMPLATE_HEAD => stack.push(token),
                        OPEN_BRACE => stack.push(token),
                        CLOSE_BRACE => {
                            if stack.last() == Some(&TEMPLATE_HEAD) {
                                if self.sc.re_scan_template_token(false) == TEMPLATE_TAIL {
                                    stack.pop();
                                }
                            } else {
                                stack.pop();
                            }
                        }
                        _ => {}
                    }
                }
                self.next_token();
            }
            if self.try_consume_declare()
                || self.try_consume_import()
                || self.try_consume_export()
                || self.try_consume_require_call(false, true)
                || self.try_consume_define()
            {
                continue;
            }
            self.next_token();
        }
    }

    /// The tail of `preProcessFile`: decides which `declare module "x"` names
    /// are added to `importedFiles`.
    fn finish(mut self) -> Vec<Vec<u16>> {
        if self.external_module {
            for (name, _depth) in self.ambient {
                self.imported.push(name);
            }
        } else {
            // Depth 0 ones become `ambientExternalModules` (not an import).
            for (name, depth) in self.ambient {
                if depth != 0 {
                    self.imported.push(name);
                }
            }
        }
        self.imported
    }
}

fn is_ascii_text(units: &[u16], ascii: &[u8]) -> bool {
    units.len() == ascii.len() && units.iter().zip(ascii).all(|(&u, &b)| u == u16::from(b))
}

// ---------------------------------------------------------------------------
// `unicodeESNextIdentifierStart` / `unicodeESNextIdentifierPart` from
// typescript.js 5.9.3 (the tables `isUnicodeIdentifierStart/Part` use for
// `languageVersion >= ES2015`), copied verbatim as inclusive [lo, hi] pairs.
// ---------------------------------------------------------------------------

static UNICODE_ESNEXT_IDENTIFIER_START: [u32; 1320] = [
    65, 90, 97, 122, 170, 170, 181, 181, 186, 186, 192, 214, 216, 246, 248, 705, 710, 721, 736, 740, 748, 748, 750, 750, 880, 884, 886,
    887, 890, 893, 895, 895, 902, 902, 904, 906, 908, 908, 910, 929, 931, 1013, 1015, 1153, 1162, 1327, 1329, 1366, 1369, 1369, 1376, 1416,
    1488, 1514, 1519, 1522, 1568, 1610, 1646, 1647, 1649, 1747, 1749, 1749, 1765, 1766, 1774, 1775, 1786, 1788, 1791, 1791, 1808, 1808,
    1810, 1839, 1869, 1957, 1969, 1969, 1994, 2026, 2036, 2037, 2042, 2042, 2048, 2069, 2074, 2074, 2084, 2084, 2088, 2088, 2112, 2136,
    2144, 2154, 2160, 2183, 2185, 2190, 2208, 2249, 2308, 2361, 2365, 2365, 2384, 2384, 2392, 2401, 2417, 2432, 2437, 2444, 2447, 2448,
    2451, 2472, 2474, 2480, 2482, 2482, 2486, 2489, 2493, 2493, 2510, 2510, 2524, 2525, 2527, 2529, 2544, 2545, 2556, 2556, 2565, 2570,
    2575, 2576, 2579, 2600, 2602, 2608, 2610, 2611, 2613, 2614, 2616, 2617, 2649, 2652, 2654, 2654, 2674, 2676, 2693, 2701, 2703, 2705,
    2707, 2728, 2730, 2736, 2738, 2739, 2741, 2745, 2749, 2749, 2768, 2768, 2784, 2785, 2809, 2809, 2821, 2828, 2831, 2832, 2835, 2856,
    2858, 2864, 2866, 2867, 2869, 2873, 2877, 2877, 2908, 2909, 2911, 2913, 2929, 2929, 2947, 2947, 2949, 2954, 2958, 2960, 2962, 2965,
    2969, 2970, 2972, 2972, 2974, 2975, 2979, 2980, 2984, 2986, 2990, 3001, 3024, 3024, 3077, 3084, 3086, 3088, 3090, 3112, 3114, 3129,
    3133, 3133, 3160, 3162, 3165, 3165, 3168, 3169, 3200, 3200, 3205, 3212, 3214, 3216, 3218, 3240, 3242, 3251, 3253, 3257, 3261, 3261,
    3293, 3294, 3296, 3297, 3313, 3314, 3332, 3340, 3342, 3344, 3346, 3386, 3389, 3389, 3406, 3406, 3412, 3414, 3423, 3425, 3450, 3455,
    3461, 3478, 3482, 3505, 3507, 3515, 3517, 3517, 3520, 3526, 3585, 3632, 3634, 3635, 3648, 3654, 3713, 3714, 3716, 3716, 3718, 3722,
    3724, 3747, 3749, 3749, 3751, 3760, 3762, 3763, 3773, 3773, 3776, 3780, 3782, 3782, 3804, 3807, 3840, 3840, 3904, 3911, 3913, 3948,
    3976, 3980, 4096, 4138, 4159, 4159, 4176, 4181, 4186, 4189, 4193, 4193, 4197, 4198, 4206, 4208, 4213, 4225, 4238, 4238, 4256, 4293,
    4295, 4295, 4301, 4301, 4304, 4346, 4348, 4680, 4682, 4685, 4688, 4694, 4696, 4696, 4698, 4701, 4704, 4744, 4746, 4749, 4752, 4784,
    4786, 4789, 4792, 4798, 4800, 4800, 4802, 4805, 4808, 4822, 4824, 4880, 4882, 4885, 4888, 4954, 4992, 5007, 5024, 5109, 5112, 5117,
    5121, 5740, 5743, 5759, 5761, 5786, 5792, 5866, 5870, 5880, 5888, 5905, 5919, 5937, 5952, 5969, 5984, 5996, 5998, 6000, 6016, 6067,
    6103, 6103, 6108, 6108, 6176, 6264, 6272, 6312, 6314, 6314, 6320, 6389, 6400, 6430, 6480, 6509, 6512, 6516, 6528, 6571, 6576, 6601,
    6656, 6678, 6688, 6740, 6823, 6823, 6917, 6963, 6981, 6988, 7043, 7072, 7086, 7087, 7098, 7141, 7168, 7203, 7245, 7247, 7258, 7293,
    7296, 7304, 7312, 7354, 7357, 7359, 7401, 7404, 7406, 7411, 7413, 7414, 7418, 7418, 7424, 7615, 7680, 7957, 7960, 7965, 7968, 8005,
    8008, 8013, 8016, 8023, 8025, 8025, 8027, 8027, 8029, 8029, 8031, 8061, 8064, 8116, 8118, 8124, 8126, 8126, 8130, 8132, 8134, 8140,
    8144, 8147, 8150, 8155, 8160, 8172, 8178, 8180, 8182, 8188, 8305, 8305, 8319, 8319, 8336, 8348, 8450, 8450, 8455, 8455, 8458, 8467,
    8469, 8469, 8472, 8477, 8484, 8484, 8486, 8486, 8488, 8488, 8490, 8505, 8508, 8511, 8517, 8521, 8526, 8526, 8544, 8584, 11264, 11492,
    11499, 11502, 11506, 11507, 11520, 11557, 11559, 11559, 11565, 11565, 11568, 11623, 11631, 11631, 11648, 11670, 11680, 11686, 11688,
    11694, 11696, 11702, 11704, 11710, 11712, 11718, 11720, 11726, 11728, 11734, 11736, 11742, 12293, 12295, 12321, 12329, 12337, 12341,
    12344, 12348, 12353, 12438, 12443, 12447, 12449, 12538, 12540, 12543, 12549, 12591, 12593, 12686, 12704, 12735, 12784, 12799, 13312,
    19903, 19968, 42124, 42192, 42237, 42240, 42508, 42512, 42527, 42538, 42539, 42560, 42606, 42623, 42653, 42656, 42735, 42775, 42783,
    42786, 42888, 42891, 42954, 42960, 42961, 42963, 42963, 42965, 42969, 42994, 43009, 43011, 43013, 43015, 43018, 43020, 43042, 43072,
    43123, 43138, 43187, 43250, 43255, 43259, 43259, 43261, 43262, 43274, 43301, 43312, 43334, 43360, 43388, 43396, 43442, 43471, 43471,
    43488, 43492, 43494, 43503, 43514, 43518, 43520, 43560, 43584, 43586, 43588, 43595, 43616, 43638, 43642, 43642, 43646, 43695, 43697,
    43697, 43701, 43702, 43705, 43709, 43712, 43712, 43714, 43714, 43739, 43741, 43744, 43754, 43762, 43764, 43777, 43782, 43785, 43790,
    43793, 43798, 43808, 43814, 43816, 43822, 43824, 43866, 43868, 43881, 43888, 44002, 44032, 55203, 55216, 55238, 55243, 55291, 63744,
    64109, 64112, 64217, 64256, 64262, 64275, 64279, 64285, 64285, 64287, 64296, 64298, 64310, 64312, 64316, 64318, 64318, 64320, 64321,
    64323, 64324, 64326, 64433, 64467, 64829, 64848, 64911, 64914, 64967, 65008, 65019, 65136, 65140, 65142, 65276, 65313, 65338, 65345,
    65370, 65382, 65470, 65474, 65479, 65482, 65487, 65490, 65495, 65498, 65500, 65536, 65547, 65549, 65574, 65576, 65594, 65596, 65597,
    65599, 65613, 65616, 65629, 65664, 65786, 65856, 65908, 66176, 66204, 66208, 66256, 66304, 66335, 66349, 66378, 66384, 66421, 66432,
    66461, 66464, 66499, 66504, 66511, 66513, 66517, 66560, 66717, 66736, 66771, 66776, 66811, 66816, 66855, 66864, 66915, 66928, 66938,
    66940, 66954, 66956, 66962, 66964, 66965, 66967, 66977, 66979, 66993, 66995, 67001, 67003, 67004, 67072, 67382, 67392, 67413, 67424,
    67431, 67456, 67461, 67463, 67504, 67506, 67514, 67584, 67589, 67592, 67592, 67594, 67637, 67639, 67640, 67644, 67644, 67647, 67669,
    67680, 67702, 67712, 67742, 67808, 67826, 67828, 67829, 67840, 67861, 67872, 67897, 67968, 68023, 68030, 68031, 68096, 68096, 68112,
    68115, 68117, 68119, 68121, 68149, 68192, 68220, 68224, 68252, 68288, 68295, 68297, 68324, 68352, 68405, 68416, 68437, 68448, 68466,
    68480, 68497, 68608, 68680, 68736, 68786, 68800, 68850, 68864, 68899, 69248, 69289, 69296, 69297, 69376, 69404, 69415, 69415, 69424,
    69445, 69488, 69505, 69552, 69572, 69600, 69622, 69635, 69687, 69745, 69746, 69749, 69749, 69763, 69807, 69840, 69864, 69891, 69926,
    69956, 69956, 69959, 69959, 69968, 70002, 70006, 70006, 70019, 70066, 70081, 70084, 70106, 70106, 70108, 70108, 70144, 70161, 70163,
    70187, 70207, 70208, 70272, 70278, 70280, 70280, 70282, 70285, 70287, 70301, 70303, 70312, 70320, 70366, 70405, 70412, 70415, 70416,
    70419, 70440, 70442, 70448, 70450, 70451, 70453, 70457, 70461, 70461, 70480, 70480, 70493, 70497, 70656, 70708, 70727, 70730, 70751,
    70753, 70784, 70831, 70852, 70853, 70855, 70855, 71040, 71086, 71128, 71131, 71168, 71215, 71236, 71236, 71296, 71338, 71352, 71352,
    71424, 71450, 71488, 71494, 71680, 71723, 71840, 71903, 71935, 71942, 71945, 71945, 71948, 71955, 71957, 71958, 71960, 71983, 71999,
    71999, 72001, 72001, 72096, 72103, 72106, 72144, 72161, 72161, 72163, 72163, 72192, 72192, 72203, 72242, 72250, 72250, 72272, 72272,
    72284, 72329, 72349, 72349, 72368, 72440, 72704, 72712, 72714, 72750, 72768, 72768, 72818, 72847, 72960, 72966, 72968, 72969, 72971,
    73008, 73030, 73030, 73056, 73061, 73063, 73064, 73066, 73097, 73112, 73112, 73440, 73458, 73474, 73474, 73476, 73488, 73490, 73523,
    73648, 73648, 73728, 74649, 74752, 74862, 74880, 75075, 77712, 77808, 77824, 78895, 78913, 78918, 82944, 83526, 92160, 92728, 92736,
    92766, 92784, 92862, 92880, 92909, 92928, 92975, 92992, 92995, 93027, 93047, 93053, 93071, 93760, 93823, 93952, 94026, 94032, 94032,
    94099, 94111, 94176, 94177, 94179, 94179, 94208, 100343, 100352, 101589, 101632, 101640, 110576, 110579, 110581, 110587, 110589,
    110590, 110592, 110882, 110898, 110898, 110928, 110930, 110933, 110933, 110948, 110951, 110960, 111355, 113664, 113770, 113776, 113788,
    113792, 113800, 113808, 113817, 119808, 119892, 119894, 119964, 119966, 119967, 119970, 119970, 119973, 119974, 119977, 119980, 119982,
    119993, 119995, 119995, 119997, 120003, 120005, 120069, 120071, 120074, 120077, 120084, 120086, 120092, 120094, 120121, 120123, 120126,
    120128, 120132, 120134, 120134, 120138, 120144, 120146, 120485, 120488, 120512, 120514, 120538, 120540, 120570, 120572, 120596, 120598,
    120628, 120630, 120654, 120656, 120686, 120688, 120712, 120714, 120744, 120746, 120770, 120772, 120779, 122624, 122654, 122661, 122666,
    122928, 122989, 123136, 123180, 123191, 123197, 123214, 123214, 123536, 123565, 123584, 123627, 124112, 124139, 124896, 124902, 124904,
    124907, 124909, 124910, 124912, 124926, 124928, 125124, 125184, 125251, 125259, 125259, 126464, 126467, 126469, 126495, 126497, 126498,
    126500, 126500, 126503, 126503, 126505, 126514, 126516, 126519, 126521, 126521, 126523, 126523, 126530, 126530, 126535, 126535, 126537,
    126537, 126539, 126539, 126541, 126543, 126545, 126546, 126548, 126548, 126551, 126551, 126553, 126553, 126555, 126555, 126557, 126557,
    126559, 126559, 126561, 126562, 126564, 126564, 126567, 126570, 126572, 126578, 126580, 126583, 126585, 126588, 126590, 126590, 126592,
    126601, 126603, 126619, 126625, 126627, 126629, 126633, 126635, 126651, 131072, 173791, 173824, 177977, 177984, 178205, 178208, 183969,
    183984, 191456, 191472, 192093, 194560, 195101, 196608, 201546, 201552, 205743,
];

static UNICODE_ESNEXT_IDENTIFIER_PART: [u32; 1538] = [
    48, 57, 65, 90, 95, 95, 97, 122, 170, 170, 181, 181, 183, 183, 186, 186, 192, 214, 216, 246, 248, 705, 710, 721, 736, 740, 748, 748,
    750, 750, 768, 884, 886, 887, 890, 893, 895, 895, 902, 906, 908, 908, 910, 929, 931, 1013, 1015, 1153, 1155, 1159, 1162, 1327, 1329,
    1366, 1369, 1369, 1376, 1416, 1425, 1469, 1471, 1471, 1473, 1474, 1476, 1477, 1479, 1479, 1488, 1514, 1519, 1522, 1552, 1562, 1568,
    1641, 1646, 1747, 1749, 1756, 1759, 1768, 1770, 1788, 1791, 1791, 1808, 1866, 1869, 1969, 1984, 2037, 2042, 2042, 2045, 2045, 2048,
    2093, 2112, 2139, 2144, 2154, 2160, 2183, 2185, 2190, 2200, 2273, 2275, 2403, 2406, 2415, 2417, 2435, 2437, 2444, 2447, 2448, 2451,
    2472, 2474, 2480, 2482, 2482, 2486, 2489, 2492, 2500, 2503, 2504, 2507, 2510, 2519, 2519, 2524, 2525, 2527, 2531, 2534, 2545, 2556,
    2556, 2558, 2558, 2561, 2563, 2565, 2570, 2575, 2576, 2579, 2600, 2602, 2608, 2610, 2611, 2613, 2614, 2616, 2617, 2620, 2620, 2622,
    2626, 2631, 2632, 2635, 2637, 2641, 2641, 2649, 2652, 2654, 2654, 2662, 2677, 2689, 2691, 2693, 2701, 2703, 2705, 2707, 2728, 2730,
    2736, 2738, 2739, 2741, 2745, 2748, 2757, 2759, 2761, 2763, 2765, 2768, 2768, 2784, 2787, 2790, 2799, 2809, 2815, 2817, 2819, 2821,
    2828, 2831, 2832, 2835, 2856, 2858, 2864, 2866, 2867, 2869, 2873, 2876, 2884, 2887, 2888, 2891, 2893, 2901, 2903, 2908, 2909, 2911,
    2915, 2918, 2927, 2929, 2929, 2946, 2947, 2949, 2954, 2958, 2960, 2962, 2965, 2969, 2970, 2972, 2972, 2974, 2975, 2979, 2980, 2984,
    2986, 2990, 3001, 3006, 3010, 3014, 3016, 3018, 3021, 3024, 3024, 3031, 3031, 3046, 3055, 3072, 3084, 3086, 3088, 3090, 3112, 3114,
    3129, 3132, 3140, 3142, 3144, 3146, 3149, 3157, 3158, 3160, 3162, 3165, 3165, 3168, 3171, 3174, 3183, 3200, 3203, 3205, 3212, 3214,
    3216, 3218, 3240, 3242, 3251, 3253, 3257, 3260, 3268, 3270, 3272, 3274, 3277, 3285, 3286, 3293, 3294, 3296, 3299, 3302, 3311, 3313,
    3315, 3328, 3340, 3342, 3344, 3346, 3396, 3398, 3400, 3402, 3406, 3412, 3415, 3423, 3427, 3430, 3439, 3450, 3455, 3457, 3459, 3461,
    3478, 3482, 3505, 3507, 3515, 3517, 3517, 3520, 3526, 3530, 3530, 3535, 3540, 3542, 3542, 3544, 3551, 3558, 3567, 3570, 3571, 3585,
    3642, 3648, 3662, 3664, 3673, 3713, 3714, 3716, 3716, 3718, 3722, 3724, 3747, 3749, 3749, 3751, 3773, 3776, 3780, 3782, 3782, 3784,
    3790, 3792, 3801, 3804, 3807, 3840, 3840, 3864, 3865, 3872, 3881, 3893, 3893, 3895, 3895, 3897, 3897, 3902, 3911, 3913, 3948, 3953,
    3972, 3974, 3991, 3993, 4028, 4038, 4038, 4096, 4169, 4176, 4253, 4256, 4293, 4295, 4295, 4301, 4301, 4304, 4346, 4348, 4680, 4682,
    4685, 4688, 4694, 4696, 4696, 4698, 4701, 4704, 4744, 4746, 4749, 4752, 4784, 4786, 4789, 4792, 4798, 4800, 4800, 4802, 4805, 4808,
    4822, 4824, 4880, 4882, 4885, 4888, 4954, 4957, 4959, 4969, 4977, 4992, 5007, 5024, 5109, 5112, 5117, 5121, 5740, 5743, 5759, 5761,
    5786, 5792, 5866, 5870, 5880, 5888, 5909, 5919, 5940, 5952, 5971, 5984, 5996, 5998, 6000, 6002, 6003, 6016, 6099, 6103, 6103, 6108,
    6109, 6112, 6121, 6155, 6157, 6159, 6169, 6176, 6264, 6272, 6314, 6320, 6389, 6400, 6430, 6432, 6443, 6448, 6459, 6470, 6509, 6512,
    6516, 6528, 6571, 6576, 6601, 6608, 6618, 6656, 6683, 6688, 6750, 6752, 6780, 6783, 6793, 6800, 6809, 6823, 6823, 6832, 6845, 6847,
    6862, 6912, 6988, 6992, 7001, 7019, 7027, 7040, 7155, 7168, 7223, 7232, 7241, 7245, 7293, 7296, 7304, 7312, 7354, 7357, 7359, 7376,
    7378, 7380, 7418, 7424, 7957, 7960, 7965, 7968, 8005, 8008, 8013, 8016, 8023, 8025, 8025, 8027, 8027, 8029, 8029, 8031, 8061, 8064,
    8116, 8118, 8124, 8126, 8126, 8130, 8132, 8134, 8140, 8144, 8147, 8150, 8155, 8160, 8172, 8178, 8180, 8182, 8188, 8204, 8205, 8255,
    8256, 8276, 8276, 8305, 8305, 8319, 8319, 8336, 8348, 8400, 8412, 8417, 8417, 8421, 8432, 8450, 8450, 8455, 8455, 8458, 8467, 8469,
    8469, 8472, 8477, 8484, 8484, 8486, 8486, 8488, 8488, 8490, 8505, 8508, 8511, 8517, 8521, 8526, 8526, 8544, 8584, 11264, 11492, 11499,
    11507, 11520, 11557, 11559, 11559, 11565, 11565, 11568, 11623, 11631, 11631, 11647, 11670, 11680, 11686, 11688, 11694, 11696, 11702,
    11704, 11710, 11712, 11718, 11720, 11726, 11728, 11734, 11736, 11742, 11744, 11775, 12293, 12295, 12321, 12335, 12337, 12341, 12344,
    12348, 12353, 12438, 12441, 12447, 12449, 12543, 12549, 12591, 12593, 12686, 12704, 12735, 12784, 12799, 13312, 19903, 19968, 42124,
    42192, 42237, 42240, 42508, 42512, 42539, 42560, 42607, 42612, 42621, 42623, 42737, 42775, 42783, 42786, 42888, 42891, 42954, 42960,
    42961, 42963, 42963, 42965, 42969, 42994, 43047, 43052, 43052, 43072, 43123, 43136, 43205, 43216, 43225, 43232, 43255, 43259, 43259,
    43261, 43309, 43312, 43347, 43360, 43388, 43392, 43456, 43471, 43481, 43488, 43518, 43520, 43574, 43584, 43597, 43600, 43609, 43616,
    43638, 43642, 43714, 43739, 43741, 43744, 43759, 43762, 43766, 43777, 43782, 43785, 43790, 43793, 43798, 43808, 43814, 43816, 43822,
    43824, 43866, 43868, 43881, 43888, 44010, 44012, 44013, 44016, 44025, 44032, 55203, 55216, 55238, 55243, 55291, 63744, 64109, 64112,
    64217, 64256, 64262, 64275, 64279, 64285, 64296, 64298, 64310, 64312, 64316, 64318, 64318, 64320, 64321, 64323, 64324, 64326, 64433,
    64467, 64829, 64848, 64911, 64914, 64967, 65008, 65019, 65024, 65039, 65056, 65071, 65075, 65076, 65101, 65103, 65136, 65140, 65142,
    65276, 65296, 65305, 65313, 65338, 65343, 65343, 65345, 65370, 65381, 65470, 65474, 65479, 65482, 65487, 65490, 65495, 65498, 65500,
    65536, 65547, 65549, 65574, 65576, 65594, 65596, 65597, 65599, 65613, 65616, 65629, 65664, 65786, 65856, 65908, 66045, 66045, 66176,
    66204, 66208, 66256, 66272, 66272, 66304, 66335, 66349, 66378, 66384, 66426, 66432, 66461, 66464, 66499, 66504, 66511, 66513, 66517,
    66560, 66717, 66720, 66729, 66736, 66771, 66776, 66811, 66816, 66855, 66864, 66915, 66928, 66938, 66940, 66954, 66956, 66962, 66964,
    66965, 66967, 66977, 66979, 66993, 66995, 67001, 67003, 67004, 67072, 67382, 67392, 67413, 67424, 67431, 67456, 67461, 67463, 67504,
    67506, 67514, 67584, 67589, 67592, 67592, 67594, 67637, 67639, 67640, 67644, 67644, 67647, 67669, 67680, 67702, 67712, 67742, 67808,
    67826, 67828, 67829, 67840, 67861, 67872, 67897, 67968, 68023, 68030, 68031, 68096, 68099, 68101, 68102, 68108, 68115, 68117, 68119,
    68121, 68149, 68152, 68154, 68159, 68159, 68192, 68220, 68224, 68252, 68288, 68295, 68297, 68326, 68352, 68405, 68416, 68437, 68448,
    68466, 68480, 68497, 68608, 68680, 68736, 68786, 68800, 68850, 68864, 68903, 68912, 68921, 69248, 69289, 69291, 69292, 69296, 69297,
    69373, 69404, 69415, 69415, 69424, 69456, 69488, 69509, 69552, 69572, 69600, 69622, 69632, 69702, 69734, 69749, 69759, 69818, 69826,
    69826, 69840, 69864, 69872, 69881, 69888, 69940, 69942, 69951, 69956, 69959, 69968, 70003, 70006, 70006, 70016, 70084, 70089, 70092,
    70094, 70106, 70108, 70108, 70144, 70161, 70163, 70199, 70206, 70209, 70272, 70278, 70280, 70280, 70282, 70285, 70287, 70301, 70303,
    70312, 70320, 70378, 70384, 70393, 70400, 70403, 70405, 70412, 70415, 70416, 70419, 70440, 70442, 70448, 70450, 70451, 70453, 70457,
    70459, 70468, 70471, 70472, 70475, 70477, 70480, 70480, 70487, 70487, 70493, 70499, 70502, 70508, 70512, 70516, 70656, 70730, 70736,
    70745, 70750, 70753, 70784, 70853, 70855, 70855, 70864, 70873, 71040, 71093, 71096, 71104, 71128, 71133, 71168, 71232, 71236, 71236,
    71248, 71257, 71296, 71352, 71360, 71369, 71424, 71450, 71453, 71467, 71472, 71481, 71488, 71494, 71680, 71738, 71840, 71913, 71935,
    71942, 71945, 71945, 71948, 71955, 71957, 71958, 71960, 71989, 71991, 71992, 71995, 72003, 72016, 72025, 72096, 72103, 72106, 72151,
    72154, 72161, 72163, 72164, 72192, 72254, 72263, 72263, 72272, 72345, 72349, 72349, 72368, 72440, 72704, 72712, 72714, 72758, 72760,
    72768, 72784, 72793, 72818, 72847, 72850, 72871, 72873, 72886, 72960, 72966, 72968, 72969, 72971, 73014, 73018, 73018, 73020, 73021,
    73023, 73031, 73040, 73049, 73056, 73061, 73063, 73064, 73066, 73102, 73104, 73105, 73107, 73112, 73120, 73129, 73440, 73462, 73472,
    73488, 73490, 73530, 73534, 73538, 73552, 73561, 73648, 73648, 73728, 74649, 74752, 74862, 74880, 75075, 77712, 77808, 77824, 78895,
    78912, 78933, 82944, 83526, 92160, 92728, 92736, 92766, 92768, 92777, 92784, 92862, 92864, 92873, 92880, 92909, 92912, 92916, 92928,
    92982, 92992, 92995, 93008, 93017, 93027, 93047, 93053, 93071, 93760, 93823, 93952, 94026, 94031, 94087, 94095, 94111, 94176, 94177,
    94179, 94180, 94192, 94193, 94208, 100343, 100352, 101589, 101632, 101640, 110576, 110579, 110581, 110587, 110589, 110590, 110592,
    110882, 110898, 110898, 110928, 110930, 110933, 110933, 110948, 110951, 110960, 111355, 113664, 113770, 113776, 113788, 113792, 113800,
    113808, 113817, 113821, 113822, 118528, 118573, 118576, 118598, 119141, 119145, 119149, 119154, 119163, 119170, 119173, 119179, 119210,
    119213, 119362, 119364, 119808, 119892, 119894, 119964, 119966, 119967, 119970, 119970, 119973, 119974, 119977, 119980, 119982, 119993,
    119995, 119995, 119997, 120003, 120005, 120069, 120071, 120074, 120077, 120084, 120086, 120092, 120094, 120121, 120123, 120126, 120128,
    120132, 120134, 120134, 120138, 120144, 120146, 120485, 120488, 120512, 120514, 120538, 120540, 120570, 120572, 120596, 120598, 120628,
    120630, 120654, 120656, 120686, 120688, 120712, 120714, 120744, 120746, 120770, 120772, 120779, 120782, 120831, 121344, 121398, 121403,
    121452, 121461, 121461, 121476, 121476, 121499, 121503, 121505, 121519, 122624, 122654, 122661, 122666, 122880, 122886, 122888, 122904,
    122907, 122913, 122915, 122916, 122918, 122922, 122928, 122989, 123023, 123023, 123136, 123180, 123184, 123197, 123200, 123209, 123214,
    123214, 123536, 123566, 123584, 123641, 124112, 124153, 124896, 124902, 124904, 124907, 124909, 124910, 124912, 124926, 124928, 125124,
    125136, 125142, 125184, 125259, 125264, 125273, 126464, 126467, 126469, 126495, 126497, 126498, 126500, 126500, 126503, 126503, 126505,
    126514, 126516, 126519, 126521, 126521, 126523, 126523, 126530, 126530, 126535, 126535, 126537, 126537, 126539, 126539, 126541, 126543,
    126545, 126546, 126548, 126548, 126551, 126551, 126553, 126553, 126555, 126555, 126557, 126557, 126559, 126559, 126561, 126562, 126564,
    126564, 126567, 126570, 126572, 126578, 126580, 126583, 126585, 126588, 126590, 126590, 126592, 126601, 126603, 126619, 126625, 126627,
    126629, 126633, 126635, 126651, 130032, 130041, 131072, 173791, 173824, 177977, 177984, 178205, 178208, 183969, 183984, 191456, 191472,
    192093, 194560, 195101, 196608, 201546, 201552, 205743, 917760, 917999,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn check(src: &str, expected: &[&str]) {
        assert_eq!(imported_files(src), expected, "source: {src:?}");
    }

    #[test]
    fn import_declarations() {
        check(r#"import a from "a";"#, &["a"]);
        check(r#"import {b} from 'b';"#, &["b"]);
        check(r#"import * as c from "c";"#, &["c"]);
        check(r#"import "d";"#, &["d"]);
        check(r#"import e, {f} from "e";"#, &["e"]);
        check(r#"import g, * as h from "g";"#, &["g"]);
        check(r#"import {} from "empty";"#, &["empty"]);
        check(r#"import { type A, B as C } from "mixed";"#, &["mixed"]);
        check(r#"import a from "a1"; import b from "b1"; import "c1""#, &["a1", "b1", "c1"]);
    }

    #[test]
    fn type_only_imports_and_exports() {
        check(r#"import type X from "t1";"#, &["t1"]);
        check(r#"import type {Y} from "t2";"#, &["t2"]);
        check(r#"import type * as Z from "t3";"#, &["t3"]);
        check(r#"import type from "t4";"#, &["t4"]);
        check(r#"import type, {a} from "t5";"#, &["t5"]);
        check(r#"export type * from "x3";"#, &["x3"]);
        check(r#"export type {T} from "x4";"#, &["x4"]);
        check(r#"export * from "x1"; export {a, b} from "x2";"#, &["x1", "x2"]);
        // `export * as ns from` is not handled by preProcessFile
        check(r#"export * as ns from "x5";"#, &[]);
        check(r#"export {a};"#, &[]);
    }

    #[test]
    fn require_and_import_equals() {
        check(r#"import x = require("r1");"#, &["r1"]);
        check(r#"export import y = require("r2");"#, &["r2"]);
        check(r#"export import type z = require("r2b");"#, &["r2b"]);
        check(r#"const z = require("r3");"#, &["r3"]);
        check("const z = require(`r4`);", &["r4"]);
        check(r#"require("r5").foo(require('r6'));"#, &["r5", "r6"]);
        check(r#"require(name);"#, &[]);
        check(r#"import x = N.M;"#, &[]);
    }

    #[test]
    fn dynamic_import() {
        check(r#"const m = await import("d1");"#, &["d1"]);
        check("const m = import(`d2`);", &["d2"]);
        check("import(`d3${x}`);", &[]);
        check("import(name);", &[]);
        check(r#"x.import("not"); import("yes");"#, &["yes"]);
        check(r#"import.meta.url; import("after");"#, &["after"]);
    }

    #[test]
    fn amd_define() {
        check(r#"define(["a", "b"], function (a, b) {});"#, &["a", "b"]);
        check(r#"define("name", ["c", `d`], function () {});"#, &["c", "d"]);
        check(r#"define(function () {});"#, &[]);
        check(r#"define("name", function () {});"#, &[]);
        check(r#"define = 1; define(["e"]);"#, &["e"]);
    }

    #[test]
    fn ambient_modules_follow_the_external_module_rule() {
        // Only a top-level `declare module "x"`: not an external module, so the
        // name is an ambient module and not an import.
        check(r#"declare module "x" { }"#, &[]);
        check(r#"declare module "x";"#, &[]);
        // Same file with a top-level import or export: it is an external module
        // and every ambient name is appended after the other imports.
        check(r#"declare module "x" { } import "a";"#, &["a", "x"]);
        check(r#"declare module "x" { } export {};"#, &["x"]);
        check(r#"declare module "x" { } export const v = 1;"#, &["x"]);
        // Nested declarations are imports even without other module syntax.
        check(r#"namespace N { declare module "y" { } }"#, &["y"]);
        check(r#"declare module "top" { } namespace N { declare module "nested" { } }"#, &["nested"]);
        // An import nested in braces does not make the file an external module.
        check(r#"declare module "x" { import "inner"; }"#, &["inner"]);
        check(r#"declare global { } declare module "g" { }"#, &[]);
        // Order: imports first, then ambient names in source order.
        check(r#"declare module "m1" { } import "i1"; declare module "m2" { } import "i2";"#, &["i1", "i2", "m1", "m2"]);
    }

    #[test]
    fn template_literals() {
        check(r#"const s = `${ import("a") } ${ {x: 1} }`; import "b";"#, &["a", "b"]);
        check(r#"const s = `a ${ `b ${ import("n") }` } c`; import "after";"#, &["n", "after"]);
        check("const s = `import \"fake\"`; import \"real\";", &["real"]);
        check(r#"const s = `${ {a: {b: 1}} }`; import "ok";"#, &["ok"]);
    }

    #[test]
    fn strings_comments_and_regex() {
        check("// import \"x\"\nimport \"y\";", &["y"]);
        check("/* import \"x\" */ import \"y\";", &["y"]);
        check("const s = 'import \"x\"'; import \"y\";", &["y"]);
        // preProcessFile never rescans `/` as a regex, so the quote inside the
        // regex starts a string literal and the quotes after it pair up wrongly.
        check(r#"const r = /"/; import "a"; import "b";"#, &[]);
        // the broken string ends at the line break, so the next line is read normally
        check("const r = /'/; import \"a\";\nimport \"b\";", &["b"]);
        check(r#"const r = /import "x"/; import "y";"#, &["x", "y"]);
        check(r#"import "unterminated"#, &["unterminated"]);
        check("import \"line\nbreak\";", &["line"]);
    }

    #[test]
    fn escapes_and_unicode() {
        check(r#"import "ab";"#, &["ab"]);
        check(r#"import "\x61\u{62}\n";"#, &["ab\n"]);
        check(
            r#"import "a\
b";"#,
            &["ab"],
        );
        // escapes inside an identifier still make it a keyword
        check(r#"import "esc";"#, &["esc"]);
        check(r#"\u{69}mport "esc2";"#, &["esc2"]);
        // non-BMP identifier part, then an import
        check("const \u{1d465} = 1; import \"nb\";", &["nb"]);
        check("const \u{4f60}\u{597d} = 1; import \"cjk\";", &["cjk"]);
    }

    #[test]
    fn identifier_escape_quirk_is_kept() {
        // scanIdentifierParts drops the plain characters between the start of
        // the parts and a later \u{...} escape, so only q1 and q3 spell `import`.
        check(r#"\u{69}\u{6d}port "q1"; \u{69}m\u{70}ort "q2"; im\u{70}ort "q3"; \u0069mp\u{6f}rt "q5";"#, &["q1", "q3"]);
    }

    #[test]
    fn identifier_characters_decide_keywords() {
        check(r#"import$ from "d1"; import_ from "d2"; $import "d3"; import "ok";"#, &["ok"]);
        check(r#"import\u0024 from "d4"; import "ok2";"#, &["ok2"]);
    }

    #[test]
    fn trivia_and_text_oddities() {
        check("#!/usr/bin/env node\nimport \"shebang\";", &["shebang"]);
        check("\u{feff}import \"bom\";", &["bom"]);
        check("import \"a\";\r\nimport \"b\";\r\n", &["a", "b"]);
        check("<<<<<<< HEAD\nimport \"ours\";\n=======\nimport \"theirs\";\n>>>>>>> branch\nimport \"after\";", &["ours", "after"]);
        // U+FFFD at a token boundary ends the scan
        check("import \"a\"; \u{fffd} import \"b\";", &["a"]);
        check("import \"a\"; \"\u{fffd}\"; import \"b\";", &["a", "b"]);
        check("", &[]);
    }

    #[test]
    fn numbers_do_not_break_scanning() {
        check("const n = 0x1F + 0b101n + 0o17 + 1_000.5e+3 + .5; import \"num\";", &["num"]);
        check("const n = 1.toString; import \"num2\";", &["num2"]);
        check("const n = 0777.5; import \"num3\";", &["num3"]);
    }

    #[test]
    fn lone_surrogate_is_replaced_in_the_str_api_only() {
        let units: Vec<u16> = "import \"a\\uD800b\";".encode_utf16().collect();
        let exact = imported_files_utf16(&units);
        assert_eq!(exact, vec![vec![0x61, 0xD800, 0x62]]);
        assert_eq!(imported_files("import \"a\\uD800b\";"), vec!["a\u{fffd}b".to_string()]);
    }

    #[test]
    fn keyword_table_covers_every_keyword_kind() {
        let mut seen = [false; (LAST_KEYWORD - FIRST_KEYWORD + 1) as usize];
        let words = [
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "debugger",
            "default",
            "delete",
            "do",
            "else",
            "enum",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "new",
            "null",
            "return",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "typeof",
            "var",
            "void",
            "while",
            "with",
            "implements",
            "interface",
            "let",
            "package",
            "private",
            "protected",
            "public",
            "static",
            "yield",
            "abstract",
            "accessor",
            "as",
            "asserts",
            "assert",
            "any",
            "async",
            "await",
            "boolean",
            "constructor",
            "declare",
            "get",
            "infer",
            "intrinsic",
            "is",
            "keyof",
            "module",
            "namespace",
            "never",
            "out",
            "readonly",
            "require",
            "number",
            "object",
            "satisfies",
            "set",
            "string",
            "symbol",
            "type",
            "undefined",
            "unique",
            "unknown",
            "using",
            "from",
            "global",
            "bigint",
            "override",
            "of",
            "defer",
        ];
        for word in words {
            let kind = keyword_kind(word.as_bytes()).unwrap_or_else(|| panic!("missing {word}"));
            assert!(is_keyword(kind));
            assert!(!seen[(kind - FIRST_KEYWORD) as usize], "duplicate kind for {word}");
            seen[(kind - FIRST_KEYWORD) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(keyword_kind(b"notakeyword"), None);
    }

    #[test]
    fn unicode_tables_are_sorted_pairs() {
        for table in [&UNICODE_ESNEXT_IDENTIFIER_START[..], &UNICODE_ESNEXT_IDENTIFIER_PART[..]] {
            assert_eq!(table.len() % 2, 0);
            assert!(table.windows(2).all(|w| w[0] <= w[1]));
        }
        assert!(is_identifier_start(0x4f60));
        assert!(!is_identifier_start(0x2028));
        assert!(is_identifier_part(0x0300));
        assert!(!is_identifier_start(0x0300));
    }
}
