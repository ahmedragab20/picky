//! `ts.readConfigFile`: tsconfig.json / jsconfig.json read the way TypeScript 5.9.3 reads them.
//!
//! This is a port of what `ts.readConfigFile(file, ts.sys.readFile).config` runs for a JSON file:
//!
//! - `ts.sys.readFile` decoding (UTF-8 BOM stripped, UTF-16 LE/BE by BOM, unreadable file -> `{}`),
//! - `parseJsonText` (the JSON-mode entry of the TypeScript parser: several top-level values become an
//!   array literal, `-` and string/number look-aheads decide how a top-level value is parsed),
//! - the scanner and the parts of the parser a value can reach (object and array literals with their
//!   list error recovery, and expressions, functions, classes, types and JSX, because a value that is
//!   not a JSON literal is still parsed as JavaScript/TypeScript and decides where the next property
//!   starts),
//! - `convertConfigFileToObject` / `convertToJson` (which values survive, which become `undefined`).
//!
//! Diagnostics are not collected, except that errors are counted: `skipParameterStart` compares the
//! error count before and after parsing a binding pattern.
//!
//! Known deviations from the TypeScript implementation:
//!
//! - A lone surrogate in a string (`"\uD800"`, or a lone surrogate in a UTF-16 file) becomes U+FFFD,
//!   because [`JsonValue::String`] holds a Rust `String`. Two keys that differ only in lone surrogates
//!   therefore collide.
//! - Nesting deeper than `MAX_DEPTH` (6000 guarded calls, about 1500 nested arrays) is a `Maximum call stack
//!   size exceeded` exception; TypeScript's own limit depends on the construct and on Node's stack (roughly
//!   800 to 8000 levels). The parser runs on a 64 MiB thread stack so that limit can't overflow the real one.
//! - TypeScript throws `Debug Failure` exceptions on a few malformed inputs and a `RangeError` on a huge hex
//!   literal; the first are reproduced (see [`try_read_config_file`]), the second is not. It also loops
//!   forever in `scanJsDocToken` on a trailing `-`; this port stops.
//! - Error counts are not de-duplicated by position the way `parseErrorAtPosition` does; no result depends on it.
//! - `JsonValue::received` knows nothing about prototypes (a `"__proto__"` object prints as `Object`).
//! - `parseErrorForMissingSemicolonAfter` reports an error unless the expression is `declare`; the
//!   spelling-suggestion corner (token `Unknown`) is not modelled. It only changes the error count
//!   compared in `skipParameterStart`.
//! - A property named `\0[[Prototype]]` collides with the slot that stores a prototype set by
//!   `"__proto__"` (see [`JsonValue::get`]).

use indexmap::IndexMap;
use std::collections::HashSet;

/// A value as TypeScript's `convertToJson` produces it; `Undefined` for a value it can't convert.
///
/// `Object` holds own properties in insertion order. A key assigned twice keeps its first position and the
/// last value, as `result[key] = value` does. JavaScript's `result["__proto__"] = value` does not create a
/// property when `value` is an object, an array or `null`: it sets the prototype. That prototype is kept in
/// a hidden entry (`PROTO_SLOT`) that [`JsonValue::get`] follows and [`JsonValue::entries`] skips.
#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(IndexMap<String, JsonValue>),
}

/// Key of the hidden entry holding an object's prototype (an `Object`, `Array` or `Null`) once
/// `"__proto__"` has set one. It cannot clash with a real property unless the JSON uses this exact key.
const PROTO_SLOT: &str = "\0[[Prototype]]";

impl JsonValue {
    /// `value?.[key]` for an object, including properties inherited through a `"__proto__"` prototype;
    /// None for a missing key or a non-object. `get("__proto__")` is the prototype when one was set (and
    /// an own `"__proto__"` property wins when there is one). An array used as prototype contributes its
    /// elements but not `length`. Properties of `Object.prototype` (`toString`, ...) are never found.
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        let JsonValue::Object(map) = self else {
            return None;
        };
        if key == PROTO_SLOT {
            return None;
        }
        let mut current = map;
        loop {
            if let Some(v) = current.get(key) {
                return Some(v);
            }
            match current.get(PROTO_SLOT) {
                Some(JsonValue::Object(parent)) => current = parent,
                Some(JsonValue::Array(items)) => {
                    if let Some(v) = canonical_index(key).and_then(|i| items.get(i)) {
                        return Some(v);
                    }
                    break;
                }
                _ => break,
            }
        }
        // `__proto__` read through Object.prototype's accessor, when the chain still reaches it
        if key == "__proto__" && reaches_object_prototype(map) { map.get(PROTO_SLOT) } else { None }
    }

    /// JavaScript truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            JsonValue::Undefined | JsonValue::Null => false,
            JsonValue::Bool(b) => *b,
            JsonValue::Number(n) => *n != 0.0 && !n.is_nan(),
            JsonValue::String(s) => !s.is_empty(),
            JsonValue::Array(_) | JsonValue::Object(_) => true,
        }
    }

    /// What Node's `ERR_INVALID_ARG_TYPE` says it received.
    pub fn received(&self) -> String {
        match self {
            JsonValue::Undefined => "Received undefined".into(),
            JsonValue::Null => "Received null".into(),
            JsonValue::Bool(b) => format!("Received type boolean ({b})"),
            JsonValue::Number(n) => {
                format!("Received type number ({})", crate::js::number_to_string(*n))
            }
            JsonValue::String(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                let shown = if units.len() > 28 { format!("{}...", String::from_utf16_lossy(&units[..25])) } else { s.clone() };
                if shown.contains('\'') {
                    format!("Received type string ({})", serde_json::Value::String(shown))
                } else {
                    format!("Received type string ('{shown}')")
                }
            }
            JsonValue::Array(_) => "Received an instance of Array".into(),
            JsonValue::Object(_) => "Received an instance of Object".into(),
        }
    }

    /// `Object.entries(value)`: integer-like keys first in ascending order, then the rest in insertion order.
    /// Own properties only (a `"__proto__"` prototype is not listed). Strings and arrays enumerate their
    /// indices.
    pub fn entries(&self) -> Vec<(String, JsonValue)> {
        match self {
            JsonValue::Object(map) => {
                let own = || map.iter().filter(|(k, _)| k.as_str() != PROTO_SLOT);
                let mut ints: Vec<(usize, &String, &JsonValue)> =
                    own().filter_map(|(k, v)| canonical_index(k).map(|n| (n, k, v))).collect();
                ints.sort_by_key(|(n, _, _)| *n);
                let mut out: Vec<(String, JsonValue)> = ints.into_iter().map(|(_, k, v)| (k.clone(), v.clone())).collect();
                out.extend(own().filter(|(k, _)| canonical_index(k).is_none()).map(|(k, v)| (k.clone(), v.clone())));
                out
            }
            JsonValue::Array(items) => items.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect(),
            JsonValue::String(s) => {
                s.encode_utf16().enumerate().map(|(i, u)| (i.to_string(), JsonValue::String(String::from_utf16_lossy(&[u])))).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// An array index: a canonical non-negative integer below 2^32 - 1 (what V8 orders first in an object).
fn canonical_index(key: &str) -> Option<usize> {
    let n: u32 = key.parse().ok()?;
    (n.to_string() == key && n != u32::MAX).then_some(n as usize)
}

/// `ts.readConfigFile(file, ts.sys.readFile).config`: `{}` when the file can't be read.
///
/// TypeScript's parser throws on a few malformed inputs (a failed `Debug` assertion, a stack overflow);
/// this returns `{}` then. [`try_read_config_file`] reports the exception instead.
pub fn read_config_file(file: &str) -> JsonValue {
    try_read_config_file(file).unwrap_or_else(|_| JsonValue::Object(IndexMap::new()))
}

/// The `config` of `ts.parseConfigFileTextToJson(name, text)` for text that is already decoded (`{}` when
/// TypeScript would throw, see [`try_parse_config_text`]).
pub fn parse_config_text(text: &str) -> JsonValue {
    try_parse_config_text(text).unwrap_or_else(|_| JsonValue::Object(IndexMap::new()))
}

/// Like [`read_config_file`], but the message of the exception TypeScript throws while parsing is an `Err`
/// (`Debug Failure. False expression.`, `Maximum call stack size exceeded`).
pub fn try_read_config_file(file: &str) -> Result<JsonValue, String> {
    match std::fs::read(file) {
        Ok(bytes) => parse_units(&decode_file(&bytes)),
        Err(_) => Ok(JsonValue::Object(IndexMap::new())),
    }
}

/// Like [`parse_config_text`], with the exception as an `Err`.
pub fn try_parse_config_text(text: &str) -> Result<JsonValue, String> {
    parse_units(&text.encode_utf16().collect::<Vec<u16>>())
}

/// `ts.sys.readFile` (Node): UTF-16 BE and LE by byte order mark, UTF-8 with the BOM dropped, invalid
/// UTF-8 replaced by U+FFFD. A byte-swapped odd trailing byte, like Node's `utf16le` decoder, is ignored.
fn decode_file(bytes: &[u8]) -> Vec<u16> {
    let units = |data: &[u8], big_endian: bool| -> Vec<u16> {
        data.as_chunks::<2>().0.iter().map(|&p| if big_endian { u16::from_be_bytes(p) } else { u16::from_le_bytes(p) }).collect()
    };
    match bytes {
        [0xFE, 0xFF, rest @ ..] => units(rest, true),
        [0xFF, 0xFE, rest @ ..] => units(rest, false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).encode_utf16().collect(),
        _ => String::from_utf8_lossy(bytes).encode_utf16().collect(),
    }
}

/// `parseConfigFileTextToJson(...).config` for UTF-16 text. The parser is recursive, so it runs on a thread
/// with a stack big enough for `MAX_DEPTH` nested calls (inline, with a lower limit, if none can be started).
fn parse_units(text: &[u16]) -> Result<JsonValue, String> {
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new().stack_size(PARSER_STACK_BYTES).spawn_scoped(scope, || parse_with_depth(text, MAX_DEPTH));
        match spawned {
            Ok(handle) => handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Err(_) => parse_with_depth(text, MAX_DEPTH_SMALL_STACK),
        }
    })
}

fn parse_with_depth(text: &[u16], max_depth: u32) -> Result<JsonValue, String> {
    let mut parser = Parser::new(text, max_depth);
    let root = parser.parse_json_text();
    if let Some(message) = parser.abort {
        return Err(message.to_string());
    }
    Ok(convert_config_file_to_object(root))
}

// ---------------------------------------------------------------------------------------------------
// convertConfigFileToObject / convertToJson
// ---------------------------------------------------------------------------------------------------

fn convert_config_file_to_object(root: Option<Node>) -> JsonValue {
    let Some(root) = root else {
        return JsonValue::Object(IndexMap::new());
    };
    if root.kind != sk::N_OBJECT_LITERAL_EXPRESSION {
        if let (sk::N_ARRAY_LITERAL_EXPRESSION, Data::Elems(elements)) = (root.kind, &root.data)
            && let Some(first) = elements.iter().find(|e| e.kind == sk::N_OBJECT_LITERAL_EXPRESSION)
        {
            return convert_property_value(first);
        }
        return JsonValue::Object(IndexMap::new());
    }
    convert_property_value(&root)
}

fn convert_property_value(node: &Node) -> JsonValue {
    match (node.kind, &node.data) {
        (sk::TRUE, _) => JsonValue::Bool(true),
        (sk::FALSE, _) => JsonValue::Bool(false),
        (sk::NULL, _) => JsonValue::Null,
        (sk::STRING_LITERAL, Data::Text(text)) => JsonValue::String(String::from_utf16_lossy(text)),
        (sk::NUMERIC_LITERAL, Data::Num(n)) => JsonValue::Number(*n),
        (sk::N_PREFIX_UNARY_EXPRESSION, Data::Child(operand)) => match (node.op, operand.kind, &operand.data) {
            (sk::MINUS, sk::NUMERIC_LITERAL, Data::Num(n)) => JsonValue::Number(-*n),
            _ => JsonValue::Undefined,
        },
        (sk::N_OBJECT_LITERAL_EXPRESSION, Data::Props(props)) => {
            let mut object = IndexMap::new();
            for prop in props {
                if prop.kind != sk::N_PROPERTY_ASSIGNMENT {
                    continue;
                }
                if let Some(key) = &prop.key {
                    assign(&mut object, String::from_utf16_lossy(key), convert_property_value(&prop.initializer));
                }
            }
            JsonValue::Object(object)
        }
        (sk::N_ARRAY_LITERAL_EXPRESSION, Data::Elems(elements)) => {
            JsonValue::Array(elements.iter().map(convert_property_value).filter(|v| *v != JsonValue::Undefined).collect())
        }
        _ => JsonValue::Undefined,
    }
}

/// Whether Object.prototype is on the prototype chain of `object`, which makes `__proto__` an accessor.
fn reaches_object_prototype(object: &IndexMap<String, JsonValue>) -> bool {
    let mut current = object;
    loop {
        match current.get(PROTO_SLOT) {
            Some(JsonValue::Object(parent)) => current = parent,
            Some(JsonValue::Null) => return false,
            _ => return true,
        }
    }
}

/// `object[key] = value` on an object made by `{}`. Only `__proto__` is special: Object.prototype's
/// setter turns an object (array) or `null` into the prototype and ignores anything else, unless the
/// prototype chain no longer reaches Object.prototype (a `null` prototype somewhere) -- then it is an
/// ordinary property.
fn assign(object: &mut IndexMap<String, JsonValue>, key: String, value: JsonValue) {
    if key != "__proto__" || object.contains_key("__proto__") || !reaches_object_prototype(object) {
        object.insert(key, value);
    } else if matches!(value, JsonValue::Object(_) | JsonValue::Array(_) | JsonValue::Null) {
        object.insert(PROTO_SLOT.to_string(), value);
    }
}

// ---------------------------------------------------------------------------------------------------
// Syntax kinds (TypeScript 5.9.3 numbering) and character classes
// ---------------------------------------------------------------------------------------------------

#[allow(dead_code)]
mod sk {
    pub type K = u16;
    pub const UNKNOWN: K = 0;
    pub const END_OF_FILE: K = 1;
    pub const SINGLE_LINE_COMMENT_TRIVIA: K = 2;
    pub const MULTI_LINE_COMMENT_TRIVIA: K = 3;
    pub const NEW_LINE_TRIVIA: K = 4;
    pub const WHITESPACE_TRIVIA: K = 5;
    pub const SHEBANG_TRIVIA: K = 6;
    pub const CONFLICT_MARKER_TRIVIA: K = 7;
    pub const NON_TEXT_FILE_MARKER_TRIVIA: K = 8;
    pub const NUMERIC_LITERAL: K = 9;
    pub const BIG_INT_LITERAL: K = 10;
    pub const STRING_LITERAL: K = 11;
    pub const JSX_TEXT: K = 12;
    pub const JSX_TEXT_ALL_WHITE_SPACES: K = 13;
    pub const REGULAR_EXPRESSION_LITERAL: K = 14;
    pub const NO_SUBSTITUTION_TEMPLATE_LITERAL: K = 15;
    pub const TEMPLATE_HEAD: K = 16;
    pub const TEMPLATE_MIDDLE: K = 17;
    pub const TEMPLATE_TAIL: K = 18;
    pub const OPEN_BRACE: K = 19;
    pub const CLOSE_BRACE: K = 20;
    pub const OPEN_PAREN: K = 21;
    pub const CLOSE_PAREN: K = 22;
    pub const OPEN_BRACKET: K = 23;
    pub const CLOSE_BRACKET: K = 24;
    pub const DOT: K = 25;
    pub const DOT_DOT_DOT: K = 26;
    pub const SEMICOLON: K = 27;
    pub const COMMA: K = 28;
    pub const QUESTION_DOT: K = 29;
    pub const LESS_THAN: K = 30;
    pub const LESS_THAN_SLASH: K = 31;
    pub const GREATER_THAN: K = 32;
    pub const LESS_THAN_EQUALS: K = 33;
    pub const GREATER_THAN_EQUALS: K = 34;
    pub const EQUALS_EQUALS: K = 35;
    pub const EXCLAMATION_EQUALS: K = 36;
    pub const EQUALS_EQUALS_EQUALS: K = 37;
    pub const EXCLAMATION_EQUALS_EQUALS: K = 38;
    pub const EQUALS_GREATER_THAN: K = 39;
    pub const PLUS: K = 40;
    pub const MINUS: K = 41;
    pub const ASTERISK: K = 42;
    pub const ASTERISK_ASTERISK: K = 43;
    pub const SLASH: K = 44;
    pub const PERCENT: K = 45;
    pub const PLUS_PLUS: K = 46;
    pub const MINUS_MINUS: K = 47;
    pub const LESS_THAN_LESS_THAN: K = 48;
    pub const GREATER_THAN_GREATER_THAN: K = 49;
    pub const GREATER_THAN_GREATER_THAN_GREATER_THAN: K = 50;
    pub const AMPERSAND: K = 51;
    pub const BAR: K = 52;
    pub const CARET: K = 53;
    pub const EXCLAMATION: K = 54;
    pub const TILDE: K = 55;
    pub const AMPERSAND_AMPERSAND: K = 56;
    pub const BAR_BAR: K = 57;
    pub const QUESTION: K = 58;
    pub const COLON: K = 59;
    pub const AT: K = 60;
    pub const QUESTION_QUESTION: K = 61;
    pub const BACKTICK: K = 62;
    pub const HASH: K = 63;
    pub const EQUALS: K = 64;
    pub const PLUS_EQUALS: K = 65;
    pub const MINUS_EQUALS: K = 66;
    pub const ASTERISK_EQUALS: K = 67;
    pub const ASTERISK_ASTERISK_EQUALS: K = 68;
    pub const SLASH_EQUALS: K = 69;
    pub const PERCENT_EQUALS: K = 70;
    pub const LESS_THAN_LESS_THAN_EQUALS: K = 71;
    pub const GREATER_THAN_GREATER_THAN_EQUALS: K = 72;
    pub const GREATER_THAN_GREATER_THAN_GREATER_THAN_EQUALS: K = 73;
    pub const AMPERSAND_EQUALS: K = 74;
    pub const BAR_EQUALS: K = 75;
    pub const BAR_BAR_EQUALS: K = 76;
    pub const AMPERSAND_AMPERSAND_EQUALS: K = 77;
    pub const QUESTION_QUESTION_EQUALS: K = 78;
    pub const CARET_EQUALS: K = 79;
    pub const IDENTIFIER: K = 80;
    pub const PRIVATE_IDENTIFIER: K = 81;
    pub const JS_DOC_COMMENT_TEXT: K = 82;
    pub const BREAK: K = 83;
    pub const CASE: K = 84;
    pub const CATCH: K = 85;
    pub const CLASS: K = 86;
    pub const CONST: K = 87;
    pub const CONTINUE: K = 88;
    pub const DEBUGGER: K = 89;
    pub const DEFAULT: K = 90;
    pub const DELETE: K = 91;
    pub const DO: K = 92;
    pub const ELSE: K = 93;
    pub const ENUM: K = 94;
    pub const EXPORT: K = 95;
    pub const EXTENDS: K = 96;
    pub const FALSE: K = 97;
    pub const FINALLY: K = 98;
    pub const FOR: K = 99;
    pub const FUNCTION: K = 100;
    pub const IF: K = 101;
    pub const IMPORT: K = 102;
    pub const IN: K = 103;
    pub const INSTANCE_OF: K = 104;
    pub const NEW: K = 105;
    pub const NULL: K = 106;
    pub const RETURN: K = 107;
    pub const SUPER: K = 108;
    pub const SWITCH: K = 109;
    pub const THIS: K = 110;
    pub const THROW: K = 111;
    pub const TRUE: K = 112;
    pub const TRY: K = 113;
    pub const TYPE_OF: K = 114;
    pub const VAR: K = 115;
    pub const VOID: K = 116;
    pub const WHILE: K = 117;
    pub const WITH: K = 118;
    pub const IMPLEMENTS: K = 119;
    pub const INTERFACE: K = 120;
    pub const LET: K = 121;
    pub const PACKAGE: K = 122;
    pub const PRIVATE: K = 123;
    pub const PROTECTED: K = 124;
    pub const PUBLIC: K = 125;
    pub const STATIC: K = 126;
    pub const YIELD: K = 127;
    pub const ABSTRACT: K = 128;
    pub const ACCESSOR: K = 129;
    pub const AS: K = 130;
    pub const ASSERTS: K = 131;
    pub const ASSERT: K = 132;
    pub const ANY: K = 133;
    pub const ASYNC: K = 134;
    pub const AWAIT: K = 135;
    pub const BOOLEAN: K = 136;
    pub const CONSTRUCTOR: K = 137;
    pub const DECLARE: K = 138;
    pub const GET: K = 139;
    pub const INFER: K = 140;
    pub const INTRINSIC: K = 141;
    pub const IS: K = 142;
    pub const KEY_OF: K = 143;
    pub const MODULE: K = 144;
    pub const NAMESPACE: K = 145;
    pub const NEVER: K = 146;
    pub const OUT: K = 147;
    pub const READONLY: K = 148;
    pub const REQUIRE: K = 149;
    pub const NUMBER: K = 150;
    pub const OBJECT: K = 151;
    pub const SATISFIES: K = 152;
    pub const SET: K = 153;
    pub const STRING: K = 154;
    pub const SYMBOL: K = 155;
    pub const TYPE: K = 156;
    pub const UNDEFINED: K = 157;
    pub const UNIQUE: K = 158;
    pub const UNKNOWN_KW: K = 159;
    pub const USING: K = 160;
    pub const FROM: K = 161;
    pub const GLOBAL: K = 162;
    pub const BIG_INT: K = 163;
    pub const OVERRIDE: K = 164;
    pub const OF: K = 165;
    pub const DEFER: K = 166;
    pub const N_QUALIFIED_NAME: K = 167;
    pub const N_COMPUTED_PROPERTY_NAME: K = 168;
    pub const N_PARAMETER: K = 170;
    pub const N_DECORATOR: K = 171;
    pub const N_PROPERTY_SIGNATURE: K = 172;
    pub const N_PROPERTY_DECLARATION: K = 173;
    pub const N_METHOD_DECLARATION: K = 175;
    pub const N_CONSTRUCTOR: K = 177;
    pub const N_GET_ACCESSOR: K = 178;
    pub const N_SET_ACCESSOR: K = 179;
    pub const N_TYPE_REFERENCE: K = 184;
    pub const N_FUNCTION_TYPE: K = 185;
    pub const N_CONSTRUCTOR_TYPE: K = 186;
    pub const N_TYPE_LITERAL: K = 188;
    pub const N_PARENTHESIZED_TYPE: K = 197;
    pub const N_ARRAY_LITERAL_EXPRESSION: K = 210;
    pub const N_OBJECT_LITERAL_EXPRESSION: K = 211;
    pub const N_PROPERTY_ACCESS_EXPRESSION: K = 212;
    pub const N_ELEMENT_ACCESS_EXPRESSION: K = 213;
    pub const N_CALL_EXPRESSION: K = 214;
    pub const N_NEW_EXPRESSION: K = 215;
    pub const N_TAGGED_TEMPLATE_EXPRESSION: K = 216;
    pub const N_PARENTHESIZED_EXPRESSION: K = 218;
    pub const N_FUNCTION_EXPRESSION: K = 219;
    pub const N_ARROW_FUNCTION: K = 220;
    pub const N_PREFIX_UNARY_EXPRESSION: K = 225;
    pub const N_POSTFIX_UNARY_EXPRESSION: K = 226;
    pub const N_BINARY_EXPRESSION: K = 227;
    pub const N_CONDITIONAL_EXPRESSION: K = 228;
    pub const N_TEMPLATE_EXPRESSION: K = 229;
    pub const N_SPREAD_ELEMENT: K = 231;
    pub const N_CLASS_EXPRESSION: K = 232;
    pub const N_OMITTED_EXPRESSION: K = 233;
    pub const N_NON_NULL_EXPRESSION: K = 236;
    pub const N_META_PROPERTY: K = 237;
    pub const N_EXPRESSION_STATEMENT: K = 245;
    pub const N_MISSING_DECLARATION: K = 283;
    pub const N_JSX_ELEMENT: K = 285;
    pub const N_JSX_SELF_CLOSING_ELEMENT: K = 286;
    pub const N_JSX_OPENING_ELEMENT: K = 287;
    pub const N_JSX_FRAGMENT: K = 289;
    pub const N_JSX_OPENING_FRAGMENT: K = 290;
    pub const N_JSX_NAMESPACED_NAME: K = 296;
    pub const N_PROPERTY_ASSIGNMENT: K = 304;
    pub const N_SHORTHAND_PROPERTY_ASSIGNMENT: K = 305;
    pub const N_SPREAD_ASSIGNMENT: K = 306;
    pub const N_JS_DOC_FUNCTION_TYPE: K = 318;
}

use sk::K;

/// Marks the nodes the parser builds that no caller looks inside (statements, most expressions, types).
const N_OTHER: K = 999;

const PRECEDING_LINE_BREAK: u32 = 1;
const UNTERMINATED: u32 = 4;
const EXTENDED_UNICODE_ESCAPE: u32 = 8;
const CONTAINS_SEPARATOR: u32 = 512;
const UNICODE_ESCAPE: u32 = 1024;
const CONTAINS_INVALID_ESCAPE: u32 = 2048;
const CONTAINS_LEADING_ZERO: u32 = 8192;
const CONTAINS_INVALID_SEPARATOR: u32 = 16384;
const PRECEDING_JSDOC_LEADING_ASTERISKS: u32 = 32768;
const OCTAL: u32 = 32;
const HEX_SPECIFIER: u32 = 64;
const BINARY_SPECIFIER: u32 = 128;
const OCTAL_SPECIFIER: u32 = 256;
const SCIENTIFIC: u32 = 16;
/// `TokenFlags.IsInvalid`: an invalid escape, a leading zero, an invalid separator or an octal literal.
const IS_INVALID: u32 = 26656;

fn is_digit(ch: i32) -> bool {
    (48..=57).contains(&ch)
}

fn is_hex_digit(ch: i32) -> bool {
    is_digit(ch) || (65..=70).contains(&ch) || (97..=102).contains(&ch)
}

fn is_octal_digit(ch: i32) -> bool {
    (48..=55).contains(&ch)
}

fn is_ascii_letter(ch: i32) -> bool {
    (65..=90).contains(&ch) || (97..=122).contains(&ch)
}

fn is_word_character(ch: i32) -> bool {
    is_ascii_letter(ch) || is_digit(ch) || ch == 95
}

fn is_line_break(ch: i32) -> bool {
    matches!(ch, 10 | 13 | 0x2028 | 0x2029)
}

fn is_white_space_single_line(ch: i32) -> bool {
    matches!(ch, 32 | 9 | 11 | 12 | 160 | 133 | 5760 | 8192..=8203 | 8239 | 8287 | 12288 | 65279)
}

fn is_white_space_like(ch: i32) -> bool {
    is_white_space_single_line(ch) || is_line_break(ch)
}

/// `lookupInUnicodeMap`: is `code` inside one of the sorted `[start, end]` pairs of `map`.
fn lookup_in_unicode_map(code: i32, map: &[u32]) -> bool {
    if code < 0 || (code as u32) < map[0] {
        return false;
    }
    let code = code as u32;
    let (mut lo, mut hi) = (0, map.len());
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

fn is_identifier_start(ch: i32) -> bool {
    is_ascii_letter(ch) || ch == 36 || ch == 95 || (ch > 127 && lookup_in_unicode_map(ch, &ID_START))
}

fn is_identifier_part(ch: i32) -> bool {
    is_word_character(ch) || ch == 36 || (ch > 127 && lookup_in_unicode_map(ch, &ID_PART))
}

/// 1 for a code unit, 2 for an astral code point, 0 for the end of the text.
fn char_size(ch: i32) -> usize {
    if ch >= 65536 {
        2
    } else if ch == -1 {
        0
    } else {
        1
    }
}

/// `textToKeyword.get(text)`
fn keyword_kind(text: &[u16]) -> Option<K> {
    if !(2..=12).contains(&text.len()) || !text.iter().all(|&u| (97..=122).contains(&u)) {
        return None;
    }
    let word = String::from_utf16_lossy(text);
    Some(match word.as_str() {
        "abstract" => sk::ABSTRACT,
        "accessor" => sk::ACCESSOR,
        "any" => sk::ANY,
        "as" => sk::AS,
        "asserts" => sk::ASSERTS,
        "assert" => sk::ASSERT,
        "bigint" => sk::BIG_INT,
        "boolean" => sk::BOOLEAN,
        "break" => sk::BREAK,
        "case" => sk::CASE,
        "catch" => sk::CATCH,
        "class" => sk::CLASS,
        "continue" => sk::CONTINUE,
        "const" => sk::CONST,
        "constructor" => sk::CONSTRUCTOR,
        "debugger" => sk::DEBUGGER,
        "declare" => sk::DECLARE,
        "default" => sk::DEFAULT,
        "defer" => sk::DEFER,
        "delete" => sk::DELETE,
        "do" => sk::DO,
        "else" => sk::ELSE,
        "enum" => sk::ENUM,
        "export" => sk::EXPORT,
        "extends" => sk::EXTENDS,
        "false" => sk::FALSE,
        "finally" => sk::FINALLY,
        "for" => sk::FOR,
        "from" => sk::FROM,
        "function" => sk::FUNCTION,
        "get" => sk::GET,
        "if" => sk::IF,
        "implements" => sk::IMPLEMENTS,
        "import" => sk::IMPORT,
        "in" => sk::IN,
        "infer" => sk::INFER,
        "instanceof" => sk::INSTANCE_OF,
        "interface" => sk::INTERFACE,
        "intrinsic" => sk::INTRINSIC,
        "is" => sk::IS,
        "keyof" => sk::KEY_OF,
        "let" => sk::LET,
        "module" => sk::MODULE,
        "namespace" => sk::NAMESPACE,
        "never" => sk::NEVER,
        "new" => sk::NEW,
        "null" => sk::NULL,
        "number" => sk::NUMBER,
        "object" => sk::OBJECT,
        "package" => sk::PACKAGE,
        "private" => sk::PRIVATE,
        "protected" => sk::PROTECTED,
        "public" => sk::PUBLIC,
        "override" => sk::OVERRIDE,
        "out" => sk::OUT,
        "readonly" => sk::READONLY,
        "require" => sk::REQUIRE,
        "global" => sk::GLOBAL,
        "return" => sk::RETURN,
        "satisfies" => sk::SATISFIES,
        "set" => sk::SET,
        "static" => sk::STATIC,
        "string" => sk::STRING,
        "super" => sk::SUPER,
        "switch" => sk::SWITCH,
        "symbol" => sk::SYMBOL,
        "this" => sk::THIS,
        "throw" => sk::THROW,
        "true" => sk::TRUE,
        "try" => sk::TRY,
        "type" => sk::TYPE,
        "typeof" => sk::TYPE_OF,
        "undefined" => sk::UNDEFINED,
        "unique" => sk::UNIQUE,
        "unknown" => sk::UNKNOWN_KW,
        "using" => sk::USING,
        "var" => sk::VAR,
        "void" => sk::VOID,
        "while" => sk::WHILE,
        "with" => sk::WITH,
        "yield" => sk::YIELD,
        "async" => sk::ASYNC,
        "await" => sk::AWAIT,
        "of" => sk::OF,
        _ => return None,
    })
}

fn is_keyword(token: K) -> bool {
    (sk::BREAK..=sk::DEFER).contains(&token)
}

fn token_is_identifier_or_keyword(token: K) -> bool {
    token >= sk::IDENTIFIER
}

/// Utf16 encoding of a code point (`String.fromCodePoint`); a surrogate code point stays a lone unit.
fn push_code_point(out: &mut Vec<u16>, code_point: u32) {
    if code_point >= 0x10000 {
        let c = code_point - 0x10000;
        out.push((0xD800 + (c >> 10)) as u16);
        out.push((0xDC00 + (c & 0x3FF)) as u16);
    } else {
        out.push(code_point as u16);
    }
}

/// `parseInt(digits, 16)` for hex digits, saturating far above U+10FFFF.
fn hex_value(digits: &[u16]) -> u64 {
    digits.iter().fold(0u64, |acc, &d| {
        let digit = (d as u8 as char).to_digit(16).unwrap_or(0) as u64;
        acc.saturating_mul(16).saturating_add(digit).min(1 << 40)
    })
}

/// The number `Number("0x...")`, `parseInt(digits, 2)` or `parseInt(digits, 8)` gives for digits of a
/// power-of-two radix: exact, rounded to nearest even.
fn power_of_two_radix_value(digits: &[u16], bits_per_digit: u32) -> f64 {
    let mut bits: Vec<bool> = Vec::new();
    for &d in digits {
        let v = (d as u8 as char).to_digit(16).unwrap_or(0);
        for i in (0..bits_per_digit).rev() {
            bits.push((v >> i) & 1 == 1);
        }
    }
    let first = bits.iter().position(|b| *b).unwrap_or(bits.len());
    let bits = &bits[first..];
    if bits.is_empty() {
        return 0.0;
    }
    if bits.len() <= 53 {
        return bits.iter().fold(0.0, |acc, b| acc * 2.0 + if *b { 1.0 } else { 0.0 });
    }
    let mut mantissa = bits[..53].iter().fold(0u64, |acc, b| acc * 2 + u64::from(*b));
    let half = bits[53];
    let sticky = bits[54..].iter().any(|b| *b);
    if half && (sticky || mantissa & 1 == 1) {
        mantissa += 1;
    }
    let exponent = bits.len() - 53;
    if exponent > 1100 {
        return f64::INFINITY;
    }
    (mantissa as f64) * 2f64.powi(exponent as i32)
}

/// `parsePseudoBigInt` for a binary or octal literal: the decimal digits of its value.
fn binary_or_octal_to_decimal(digits: &[u16], bits_per_digit: u32) -> Vec<u16> {
    // little-endian base 10^9 limbs
    let mut limbs: Vec<u64> = vec![0];
    for &d in digits {
        let v = (d as u8 as char).to_digit(16).unwrap_or(0) as u64;
        let mut carry = v;
        for limb in limbs.iter_mut() {
            let x = (*limb << bits_per_digit) + carry;
            *limb = x % 1_000_000_000;
            carry = x / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut text = String::new();
    for (i, limb) in limbs.iter().rev().enumerate() {
        if i == 0 {
            text.push_str(&limb.to_string());
        } else {
            text.push_str(&format!("{limb:09}"));
        }
    }
    text.encode_utf16().collect()
}

// ---------------------------------------------------------------------------------------------------
// Syntax tree: only what the callers of the parser look at
// ---------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Node {
    kind: K,
    pos: usize,
    end: usize,
    /// The operator of a prefix unary expression; 1 on a type reference without a name and on a
    /// function or constructor type without a parameter list.
    op: K,
    data: Data,
}

impl Node {
    /// `nodeIsMissing`
    fn is_missing(&self) -> bool {
        self.pos == self.end
    }
}

#[derive(Clone, Debug, Default)]
enum Data {
    #[default]
    None,
    /// Cooked text of a string literal, text of an identifier or a bigint literal.
    Text(Vec<u16>),
    Num(f64),
    Child(Box<Node>),
    Elems(Vec<Node>),
    Props(Vec<Prop>),
    /// Opening and closing tag name of a JSX element (an opening element has no closing tag).
    Tags(Box<(Tag, Tag)>),
}

/// An element of an object literal.
#[derive(Clone, Debug)]
struct Prop {
    kind: K,
    /// `getTextOfPropertyName`, None for a computed name that is not a literal.
    key: Option<Vec<u16>>,
    initializer: Node,
}

/// A JSX tag name, for `tagNamesAreEquivalent`.
#[derive(Clone, Debug, PartialEq)]
enum Tag {
    Identifier(Vec<u16>),
    This,
    Namespaced(Vec<u16>, Vec<u16>),
    Access(Box<Tag>, Vec<u16>),
}

/// How a modifier list looks: `list && createNodeArray(list)` is undefined when empty.
#[derive(Clone, Debug, Default)]
struct Modifiers {
    kinds: Vec<K>,
}

impl Modifiers {
    fn some(&self) -> bool {
        !self.kinds.is_empty()
    }

    fn has(&self, kind: K) -> bool {
        self.kinds.contains(&kind)
    }
}

// ---------------------------------------------------------------------------------------------------
// Parser state, speculation, the scanner
// ---------------------------------------------------------------------------------------------------

/// Context flags the parser consults (`NodeFlags`); `JavaScriptFile` is always set for a JSON file.
const CTX_DISALLOW_IN: u32 = 8192;
const CTX_YIELD: u32 = 16384;
const CTX_DECORATOR: u32 = 32768;
const CTX_AWAIT: u32 = 65536;
const CTX_DISALLOW_CONDITIONAL_TYPES: u32 = 131072;
const CTX_TYPE_EXCLUDES: u32 = CTX_YIELD | CTX_AWAIT;

/// `ParsingContext`, as the bit position in `Parser::parsing_context`.
mod pc {
    pub const SOURCE_ELEMENTS: u32 = 0;
    pub const BLOCK_STATEMENTS: u32 = 1;
    pub const SWITCH_CLAUSES: u32 = 2;
    pub const SWITCH_CLAUSE_STATEMENTS: u32 = 3;
    pub const TYPE_MEMBERS: u32 = 4;
    pub const CLASS_MEMBERS: u32 = 5;
    pub const ENUM_MEMBERS: u32 = 6;
    pub const HERITAGE_CLAUSE_ELEMENT: u32 = 7;
    pub const VARIABLE_DECLARATIONS: u32 = 8;
    pub const OBJECT_BINDING_ELEMENTS: u32 = 9;
    pub const ARRAY_BINDING_ELEMENTS: u32 = 10;
    pub const ARGUMENT_EXPRESSIONS: u32 = 11;
    pub const OBJECT_LITERAL_MEMBERS: u32 = 12;
    pub const JSX_ATTRIBUTES: u32 = 13;
    pub const JSX_CHILDREN: u32 = 14;
    pub const ARRAY_LITERAL_MEMBERS: u32 = 15;
    pub const PARAMETERS: u32 = 16;
    pub const JSDOC_PARAMETERS: u32 = 17;
    pub const REST_PROPERTIES: u32 = 18;
    pub const TYPE_PARAMETERS: u32 = 19;
    pub const TYPE_ARGUMENTS: u32 = 20;
    pub const TUPLE_ELEMENT_TYPES: u32 = 21;
    pub const HERITAGE_CLAUSES: u32 = 22;
    pub const IMPORT_OR_EXPORT_SPECIFIERS: u32 = 23;
    pub const IMPORT_ATTRIBUTES: u32 = 24;
    pub const COUNT: u32 = 26;
}

/// How many guarded (recursive) parser calls may be active at once before the parse stops with `Maximum call
/// stack size exceeded`. A nested array costs 4 of them, so about 1500 levels fit; the parser runs on a stack
/// of `PARSER_STACK_BYTES`, which holds `MAX_DEPTH` of the heaviest frames with room to spare.
const MAX_DEPTH: u32 = 6000;

/// The limit used when no thread with a big stack can be started (about 100 levels of nesting).
const MAX_DEPTH_SMALL_STACK: u32 = 400;

const PARSER_STACK_BYTES: usize = 64 << 20;

/// A snapshot for `lookAhead` / `tryParse`: the scanner state, the parser's current token and the error count.
struct Snapshot {
    pos: usize,
    full_start: usize,
    token_start: usize,
    token: K,
    token_value: Vec<u16>,
    token_flags: u32,
    current: K,
    errors: usize,
}

struct Parser<'a> {
    text: &'a [u16],
    // scanner
    pos: usize,
    full_start: usize,
    token_start: usize,
    /// The scanner's token (the parser's `token()` is `current`; they differ after `reScanAsteriskEqualsToken`).
    token: K,
    token_value: Vec<u16>,
    token_flags: u32,
    skip_jsdoc_leading_asterisks: i32,
    // parser
    current: K,
    context_flags: u32,
    parsing_context: u32,
    /// Number of diagnostics reported so far.
    errors: usize,
    not_parenthesized_arrow: HashSet<usize>,
    depth: u32,
    max_depth: u32,
    /// The exception TypeScript would have thrown (a `Debug` failure or a stack overflow); once set, the
    /// rest of the text is skipped.
    abort: Option<&'static str>,
}

impl<'a> Parser<'a> {
    fn new(text: &'a [u16], max_depth: u32) -> Self {
        Parser {
            text,
            pos: 0,
            full_start: 0,
            token_start: 0,
            token: sk::UNKNOWN,
            token_value: Vec::new(),
            token_flags: 0,
            skip_jsdoc_leading_asterisks: 0,
            current: sk::UNKNOWN,
            context_flags: 0,
            parsing_context: 0,
            errors: 0,
            not_parenthesized_arrow: HashSet::new(),
            depth: 0,
            max_depth,
            abort: None,
        }
    }

    // ----- scanner: characters -----

    fn end(&self) -> usize {
        self.text.len()
    }

    /// `charCodeUnchecked(pos)`: -1 outside the text.
    fn cc(&self, pos: usize) -> i32 {
        self.text.get(pos).map_or(-1, |&u| i32::from(u))
    }

    /// `charCodeUnchecked(pos - 1)`
    fn cc_before(&self, pos: usize) -> i32 {
        if pos == 0 { -1 } else { self.cc(pos - 1) }
    }

    /// `codePointUnchecked(pos)`: -1 outside the text.
    fn cp(&self, pos: usize) -> i32 {
        let Some(&first) = self.text.get(pos) else {
            return -1;
        };
        if (0xD800..0xDC00).contains(&first)
            && let Some(&second) = self.text.get(pos + 1)
            && (0xDC00..0xE000).contains(&second)
        {
            return 0x10000 + ((i32::from(first) - 0xD800) << 10) + (i32::from(second) - 0xDC00);
        }
        i32::from(first)
    }

    fn substring(&self, from: usize, to: usize) -> &[u16] {
        &self.text[from.min(self.end())..to.min(self.end())]
    }

    /// A diagnostic. Only counted: see the module documentation.
    fn error(&mut self) {
        self.errors += 1;
    }

    // ----- scanner: numbers -----

    fn scan_number_fragment(&mut self) -> Vec<u16> {
        let mut start = self.pos;
        let mut allow_separator = false;
        let mut result = Vec::new();
        loop {
            let ch = self.cc(self.pos);
            if ch == 95 {
                self.token_flags |= CONTAINS_SEPARATOR;
                if allow_separator {
                    allow_separator = false;
                    result.extend_from_slice(self.substring(start, self.pos));
                } else {
                    self.token_flags |= CONTAINS_INVALID_SEPARATOR;
                    self.error();
                }
                self.pos += 1;
                start = self.pos;
                continue;
            }
            if is_digit(ch) {
                allow_separator = true;
                self.pos += 1;
                continue;
            }
            break;
        }
        if self.cc_before(self.pos) == 95 {
            self.token_flags |= CONTAINS_INVALID_SEPARATOR;
            self.error();
        }
        result.extend_from_slice(self.substring(start, self.pos));
        result
    }

    /// `scanNumber`; leaves the text of the number in `token_value` and returns the token kind.
    fn scan_number(&mut self) -> K {
        let start = self.pos;
        let main_fragment;
        if self.cc(self.pos) == 48 {
            self.pos += 1;
            if self.cc(self.pos) == 95 {
                self.token_flags |= CONTAINS_SEPARATOR | CONTAINS_INVALID_SEPARATOR;
                self.error();
                self.pos -= 1;
                main_fragment = self.scan_number_fragment();
            } else if !self.scan_digits() {
                self.token_flags |= CONTAINS_LEADING_ZERO;
                main_fragment = js_number_text(&self.token_value);
            } else if self.token_value.is_empty() {
                main_fragment = vec![48];
            } else {
                // "0" followed by octal digits only
                let value = power_of_two_radix_value(&self.token_value, 3);
                self.token_value = crate::js::number_to_string(value).encode_utf16().collect();
                self.token_flags |= OCTAL;
                self.error();
                return sk::NUMERIC_LITERAL;
            }
        } else {
            main_fragment = self.scan_number_fragment();
        }
        let mut decimal_fragment: Option<Vec<u16>> = None;
        if self.cc(self.pos) == 46 {
            self.pos += 1;
            decimal_fragment = Some(self.scan_number_fragment());
        }
        let mut end = self.pos;
        let mut scientific_fragment: Option<Vec<u16>> = None;
        if self.cc(self.pos) == 69 || self.cc(self.pos) == 101 {
            self.pos += 1;
            self.token_flags |= SCIENTIFIC;
            if self.cc(self.pos) == 43 || self.cc(self.pos) == 45 {
                self.pos += 1;
            }
            let pre_numeric_part = self.pos;
            let final_fragment = self.scan_number_fragment();
            if final_fragment.is_empty() {
                self.error();
            } else {
                let mut fragment = self.substring(end, pre_numeric_part).to_vec();
                fragment.extend_from_slice(&final_fragment);
                scientific_fragment = Some(fragment);
                end = self.pos;
            }
        }
        let result: Vec<u16> = if self.token_flags & CONTAINS_SEPARATOR != 0 {
            let mut r = main_fragment;
            if let Some(d) = decimal_fragment.as_ref().filter(|d| !d.is_empty()) {
                r.push(46);
                r.extend_from_slice(d);
            }
            if let Some(s) = &scientific_fragment {
                r.extend_from_slice(s);
            }
            r
        } else {
            self.substring(start, end).to_vec()
        };
        if self.token_flags & CONTAINS_LEADING_ZERO != 0 {
            self.error();
            self.token_value = js_number_text(&result);
            return sk::NUMERIC_LITERAL;
        }
        if decimal_fragment.is_some() || self.token_flags & SCIENTIFIC != 0 {
            self.check_for_identifier_start_after_numeric_literal();
            self.token_value = js_number_text(&result);
            sk::NUMERIC_LITERAL
        } else {
            self.token_value = result;
            let kind = self.check_big_int_suffix();
            self.check_for_identifier_start_after_numeric_literal();
            kind
        }
    }

    fn check_for_identifier_start_after_numeric_literal(&mut self) {
        if !is_identifier_start(self.cp(self.pos)) {
            return;
        }
        let identifier_start = self.pos;
        let length = self.scan_identifier_parts().len();
        // a lone `n` after a number with a fraction or exponent is a bigint error and stays consumed; any
        // other identifier is reported and left for the next token
        let is_big_int_n = length == 1 && self.cc(identifier_start) == 110;
        self.error();
        if !is_big_int_n {
            self.pos = identifier_start;
        }
    }

    /// `scanDigits`: leaves the digits in `token_value`; true when they are all octal digits.
    fn scan_digits(&mut self) -> bool {
        let start = self.pos;
        let mut is_octal = true;
        while is_digit(self.cc(self.pos)) {
            if !is_octal_digit(self.cc(self.pos)) {
                is_octal = false;
            }
            self.pos += 1;
        }
        self.token_value = self.substring(start, self.pos).to_vec();
        is_octal
    }

    /// `scanHexDigits`: the digits found (lower case) or nothing when there are fewer than `min_count`.
    fn scan_hex_digits(&mut self, min_count: usize, scan_as_many_as_possible: bool, can_have_separators: bool) -> Vec<u16> {
        let mut value: Vec<u16> = Vec::new();
        let mut allow_separator = false;
        while value.len() < min_count || scan_as_many_as_possible {
            let mut ch = self.cc(self.pos);
            if can_have_separators && ch == 95 {
                self.token_flags |= CONTAINS_SEPARATOR;
                if allow_separator {
                    allow_separator = false;
                } else {
                    self.error();
                }
                self.pos += 1;
                continue;
            }
            allow_separator = can_have_separators;
            if (65..=70).contains(&ch) {
                ch += 32;
            } else if !((48..=57).contains(&ch) || (97..=102).contains(&ch)) {
                break;
            }
            value.push(ch as u16);
            self.pos += 1;
        }
        if value.len() < min_count {
            value.clear();
        }
        if self.cc_before(self.pos) == 95 {
            self.error();
        }
        value
    }

    fn scan_exact_number_of_hex_digits(&mut self, count: usize) -> i64 {
        let digits = self.scan_hex_digits(count, false, false);
        if digits.is_empty() { -1 } else { hex_value(&digits) as i64 }
    }

    fn scan_binary_or_octal_digits(&mut self, base: i32) -> Vec<u16> {
        let mut value = Vec::new();
        let mut separator_allowed = false;
        loop {
            let ch = self.cc(self.pos);
            if ch == 95 {
                self.token_flags |= CONTAINS_SEPARATOR;
                if separator_allowed {
                    separator_allowed = false;
                } else {
                    self.error();
                }
                self.pos += 1;
                continue;
            }
            separator_allowed = true;
            if !is_digit(ch) || ch - 48 >= base {
                break;
            }
            value.push(ch as u16);
            self.pos += 1;
        }
        if self.cc_before(self.pos) == 95 {
            self.error();
        }
        value
    }

    /// `checkBigIntSuffix`: `n` makes a bigint literal; otherwise `token_value` becomes the number's text.
    fn check_big_int_suffix(&mut self) -> K {
        if self.cc(self.pos) == 110 {
            if self.token_flags & (BINARY_SPECIFIER | OCTAL_SPECIFIER) != 0 {
                let bits = if self.token_flags & BINARY_SPECIFIER != 0 { 1 } else { 3 };
                self.token_value = binary_or_octal_to_decimal(&self.token_value[2..], bits);
            }
            self.token_value.push(110);
            self.pos += 1;
            return sk::BIG_INT_LITERAL;
        }
        let tv = &self.token_value;
        let value = if self.token_flags & BINARY_SPECIFIER != 0 {
            power_of_two_radix_value(&tv[2..], 1)
        } else if self.token_flags & OCTAL_SPECIFIER != 0 {
            power_of_two_radix_value(&tv[2..], 3)
        } else if self.token_flags & HEX_SPECIFIER != 0 {
            power_of_two_radix_value(&tv[2..], 4)
        } else {
            js_number_value(tv)
        };
        self.token_value = crate::js::number_to_string(value).encode_utf16().collect();
        sk::NUMERIC_LITERAL
    }

    // ----- scanner: strings, templates, escapes, identifiers -----

    fn scan_string(&mut self, jsx_attribute_string: bool) -> Vec<u16> {
        let quote = self.cc(self.pos);
        self.pos += 1;
        let mut result = Vec::new();
        let mut start = self.pos;
        loop {
            if self.pos >= self.end() {
                result.extend_from_slice(self.substring(start, self.pos));
                self.token_flags |= UNTERMINATED;
                self.error();
                break;
            }
            let ch = self.cc(self.pos);
            if ch == quote {
                result.extend_from_slice(self.substring(start, self.pos));
                self.pos += 1;
                break;
            }
            if ch == 92 && !jsx_attribute_string {
                result.extend_from_slice(self.substring(start, self.pos));
                self.scan_escape_sequence(true, &mut result);
                start = self.pos;
                continue;
            }
            if (ch == 10 || ch == 13) && !jsx_attribute_string {
                result.extend_from_slice(self.substring(start, self.pos));
                self.token_flags |= UNTERMINATED;
                self.error();
                break;
            }
            self.pos += 1;
        }
        result
    }

    /// `scanTemplateAndSetTokenValue`
    fn scan_template_and_set_token_value(&mut self, should_emit_invalid_escape_error: bool) -> K {
        let started_with_backtick = self.cc(self.pos) == 96;
        self.pos += 1;
        let mut start = self.pos;
        let mut contents: Vec<u16> = Vec::new();
        let resulting_token;
        loop {
            if self.pos >= self.end() {
                contents.extend_from_slice(self.substring(start, self.pos));
                self.token_flags |= UNTERMINATED;
                self.error();
                resulting_token = if started_with_backtick { sk::NO_SUBSTITUTION_TEMPLATE_LITERAL } else { sk::TEMPLATE_TAIL };
                break;
            }
            let curr = self.cc(self.pos);
            if curr == 96 {
                contents.extend_from_slice(self.substring(start, self.pos));
                self.pos += 1;
                resulting_token = if started_with_backtick { sk::NO_SUBSTITUTION_TEMPLATE_LITERAL } else { sk::TEMPLATE_TAIL };
                break;
            }
            if curr == 36 && self.pos + 1 < self.end() && self.cc(self.pos + 1) == 123 {
                contents.extend_from_slice(self.substring(start, self.pos));
                self.pos += 2;
                resulting_token = if started_with_backtick { sk::TEMPLATE_HEAD } else { sk::TEMPLATE_MIDDLE };
                break;
            }
            if curr == 92 {
                contents.extend_from_slice(self.substring(start, self.pos));
                self.scan_escape_sequence(should_emit_invalid_escape_error, &mut contents);
                start = self.pos;
                continue;
            }
            if curr == 13 {
                contents.extend_from_slice(self.substring(start, self.pos));
                self.pos += 1;
                if self.pos < self.end() && self.cc(self.pos) == 10 {
                    self.pos += 1;
                }
                contents.push(10);
                start = self.pos;
                continue;
            }
            self.pos += 1;
        }
        self.token_value = contents;
        resulting_token
    }

    /// `scanEscapeSequence` for strings and templates (`flags` String | ReportErrors, or String alone).
    fn scan_escape_sequence(&mut self, report_errors: bool, out: &mut Vec<u16>) {
        let start = self.pos;
        self.pos += 1;
        if self.pos >= self.end() {
            self.error();
            return;
        }
        let ch = self.cc(self.pos);
        self.pos += 1;
        match ch {
            48..=55 => {
                if ch == 48 && (self.pos >= self.end() || !is_digit(self.cc(self.pos))) {
                    out.push(0);
                    return;
                }
                if ch <= 51 && self.pos < self.end() && is_octal_digit(self.cc(self.pos)) {
                    self.pos += 1;
                }
                if self.pos < self.end() && is_octal_digit(self.cc(self.pos)) {
                    self.pos += 1;
                }
                self.token_flags |= CONTAINS_INVALID_ESCAPE;
                if report_errors {
                    let code = self.substring(start + 1, self.pos).iter().fold(0u32, |acc, &d| acc * 8 + u32::from(d - 48));
                    self.error();
                    out.push(code as u16);
                } else {
                    out.extend_from_slice(self.substring(start, self.pos));
                }
            }
            56 | 57 => {
                self.token_flags |= CONTAINS_INVALID_ESCAPE;
                if report_errors {
                    self.error();
                    out.push(ch as u16);
                } else {
                    out.extend_from_slice(self.substring(start, self.pos));
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
                if self.pos < self.end() && self.cc(self.pos) == 123 {
                    self.pos -= 2;
                    self.scan_extended_unicode_escape(report_errors, out);
                    return;
                }
                while self.pos < start + 6 {
                    if !(self.pos < self.end() && is_hex_digit(self.cc(self.pos))) {
                        self.token_flags |= CONTAINS_INVALID_ESCAPE;
                        if report_errors {
                            self.error();
                        }
                        out.extend_from_slice(self.substring(start, self.pos));
                        return;
                    }
                    self.pos += 1;
                }
                self.token_flags |= UNICODE_ESCAPE;
                let digits: Vec<u16> = self.substring(start + 2, self.pos).to_vec();
                out.push(hex_value(&digits) as u16);
            }
            120 => {
                while self.pos < start + 4 {
                    if !(self.pos < self.end() && is_hex_digit(self.cc(self.pos))) {
                        self.token_flags |= CONTAINS_INVALID_ESCAPE;
                        if report_errors {
                            self.error();
                        }
                        out.extend_from_slice(self.substring(start, self.pos));
                        return;
                    }
                    self.pos += 1;
                }
                self.token_flags |= 4096;
                let digits: Vec<u16> = self.substring(start + 2, self.pos).to_vec();
                out.push(hex_value(&digits) as u16);
            }
            13 => {
                if self.pos < self.end() && self.cc(self.pos) == 10 {
                    self.pos += 1;
                }
            }
            10 | 0x2028 | 0x2029 => {}
            _ => out.push(ch as u16),
        }
    }

    /// `scanExtendedUnicodeEscape`, appending the character (or the raw text when it is invalid).
    fn scan_extended_unicode_escape(&mut self, should_emit_invalid_escape_error: bool, out: &mut Vec<u16>) {
        let start = self.pos;
        self.pos += 3;
        let digits = self.scan_hex_digits(1, true, false);
        let value: i64 = if digits.is_empty() { -1 } else { hex_value(&digits) as i64 };
        let mut invalid = false;
        if !(0..=1_114_111).contains(&value) {
            if should_emit_invalid_escape_error {
                self.error();
            }
            invalid = true;
        }
        if self.pos >= self.end() {
            if should_emit_invalid_escape_error {
                self.error();
            }
            invalid = true;
        } else if self.cc(self.pos) == 125 {
            self.pos += 1;
        } else {
            if should_emit_invalid_escape_error {
                self.error();
            }
            invalid = true;
        }
        if invalid {
            self.token_flags |= CONTAINS_INVALID_ESCAPE;
            out.extend_from_slice(self.substring(start, self.pos));
            return;
        }
        self.token_flags |= EXTENDED_UNICODE_ESCAPE;
        push_code_point(out, value as u32);
    }

    /// `peekUnicodeEscape`: the value of a `\uXXXX` at `pos`, or -1.
    fn peek_unicode_escape(&mut self) -> i64 {
        if self.pos + 5 < self.end() && self.cc(self.pos + 1) == 117 {
            let start = self.pos;
            self.pos += 2;
            let value = self.scan_exact_number_of_hex_digits(4);
            self.pos = start;
            return value;
        }
        -1
    }

    /// `peekExtendedUnicodeEscape`: the value of a `\u{...}` at `pos`, or -1.
    fn peek_extended_unicode_escape(&mut self) -> i64 {
        if self.cp(self.pos + 1) == 117 && self.cp(self.pos + 2) == 123 {
            let start = self.pos;
            self.pos += 3;
            let digits = self.scan_hex_digits(1, true, false);
            let value = if digits.is_empty() { -1 } else { hex_value(&digits) as i64 };
            self.pos = start;
            return value;
        }
        -1
    }

    /// `scanIdentifierParts`
    fn scan_identifier_parts(&mut self) -> Vec<u16> {
        let mut result = Vec::new();
        let mut start = self.pos;
        while self.pos < self.end() {
            let ch = self.cp(self.pos);
            if is_identifier_part(ch) {
                self.pos += char_size(ch);
            } else if ch == 92 {
                let extended = self.peek_extended_unicode_escape();
                if extended >= 0 && is_identifier_part(extended.min(i64::from(i32::MAX)) as i32) {
                    self.scan_extended_unicode_escape(true, &mut result);
                    start = self.pos;
                    continue;
                }
                let value = self.peek_unicode_escape();
                if !(value >= 0 && is_identifier_part(value as i32)) {
                    break;
                }
                self.token_flags |= UNICODE_ESCAPE;
                result.extend_from_slice(self.substring(start, self.pos));
                push_code_point(&mut result, value as u32);
                self.pos += 6;
                start = self.pos;
            } else {
                break;
            }
        }
        result.extend_from_slice(self.substring(start, self.pos));
        result
    }

    /// `getIdentifierToken`
    fn get_identifier_token(&mut self) -> K {
        self.token = keyword_kind(&self.token_value).unwrap_or(sk::IDENTIFIER);
        self.token
    }

    /// `scanIdentifier`: Some(token) when an identifier starts at `start_character`.
    fn scan_identifier(&mut self, start_character: i32) -> Option<K> {
        let mut ch = start_character;
        if !is_identifier_start(ch) {
            return None;
        }
        self.pos += char_size(ch);
        while self.pos < self.end() {
            ch = self.cp(self.pos);
            if !is_identifier_part(ch) {
                break;
            }
            self.pos += char_size(ch);
        }
        self.token_value = self.substring(self.token_start, self.pos).to_vec();
        if self.pos < self.end() && ch == 92 {
            let rest = self.scan_identifier_parts();
            self.token_value.extend(rest);
        }
        Some(self.get_identifier_token())
    }

    // ----- scanner: trivia -----

    fn is_conflict_marker_trivia(&self, pos: usize) -> bool {
        if pos == 0 || is_line_break(self.cc(pos - 1)) {
            let ch = self.cc(pos);
            if pos + 7 < self.end() {
                for i in 0..7 {
                    if self.cc(pos + i) != ch {
                        return false;
                    }
                }
                return ch == 61 || self.cc(pos + 7) == 32;
            }
        }
        false
    }

    fn scan_conflict_marker_trivia(&mut self, mut pos: usize) -> usize {
        self.error();
        let ch = self.cc(pos);
        let len = self.end();
        if ch == 60 || ch == 62 {
            while pos < len && !is_line_break(self.cc(pos)) {
                pos += 1;
            }
        } else {
            while pos < len {
                let current = self.cc(pos);
                if (current == 61 || current == 62) && current != ch && self.is_conflict_marker_trivia(pos) {
                    break;
                }
                pos += 1;
            }
        }
        pos
    }

    /// `isShebangTrivia(text, 0)`: the text starts with `#!`.
    fn is_shebang_trivia(&self) -> bool {
        self.cc(0) == 35 && self.cc(1) == 33
    }

    // ----- scanner: scan -----

    /// `scan()` with `skipTrivia` on.
    fn scan(&mut self) -> K {
        self.full_start = self.pos;
        self.token_flags = 0;
        loop {
            self.token_start = self.pos;
            if self.pos >= self.end() {
                self.token = sk::END_OF_FILE;
                return self.token;
            }
            let ch = self.cp(self.pos);
            if self.pos == 0 && ch == 35 && self.is_shebang_trivia() {
                while self.pos < self.end() && !is_line_break(self.cc(self.pos)) {
                    self.pos += 1;
                }
                continue;
            }
            macro_rules! tok {
                ($len:expr, $kind:expr) => {{
                    self.pos += $len;
                    self.token = $kind;
                    return self.token;
                }};
            }
            match ch {
                10 | 13 => {
                    self.token_flags |= PRECEDING_LINE_BREAK;
                    self.pos += 1;
                    continue;
                }
                9 | 11 | 12 | 32 | 160 | 5760 | 8192..=8203 | 8239 | 8287 | 12288 | 65279 => {
                    self.pos += 1;
                    continue;
                }
                33 => {
                    if self.cc(self.pos + 1) == 61 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::EXCLAMATION_EQUALS_EQUALS);
                        }
                        tok!(2, sk::EXCLAMATION_EQUALS);
                    }
                    tok!(1, sk::EXCLAMATION);
                }
                34 | 39 => {
                    self.token_value = self.scan_string(false);
                    self.token = sk::STRING_LITERAL;
                    return self.token;
                }
                96 => {
                    self.token = self.scan_template_and_set_token_value(false);
                    return self.token;
                }
                37 => {
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::PERCENT_EQUALS);
                    }
                    tok!(1, sk::PERCENT);
                }
                38 => {
                    if self.cc(self.pos + 1) == 38 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::AMPERSAND_AMPERSAND_EQUALS);
                        }
                        tok!(2, sk::AMPERSAND_AMPERSAND);
                    }
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::AMPERSAND_EQUALS);
                    }
                    tok!(1, sk::AMPERSAND);
                }
                40 => tok!(1, sk::OPEN_PAREN),
                41 => tok!(1, sk::CLOSE_PAREN),
                42 => {
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::ASTERISK_EQUALS);
                    }
                    if self.cc(self.pos + 1) == 42 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::ASTERISK_ASTERISK_EQUALS);
                        }
                        tok!(2, sk::ASTERISK_ASTERISK);
                    }
                    self.pos += 1;
                    if self.skip_jsdoc_leading_asterisks != 0
                        && self.token_flags & PRECEDING_JSDOC_LEADING_ASTERISKS == 0
                        && self.token_flags & PRECEDING_LINE_BREAK != 0
                    {
                        self.token_flags |= PRECEDING_JSDOC_LEADING_ASTERISKS;
                        continue;
                    }
                    self.token = sk::ASTERISK;
                    return self.token;
                }
                43 => {
                    if self.cc(self.pos + 1) == 43 {
                        tok!(2, sk::PLUS_PLUS);
                    }
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::PLUS_EQUALS);
                    }
                    tok!(1, sk::PLUS);
                }
                44 => tok!(1, sk::COMMA),
                45 => {
                    if self.cc(self.pos + 1) == 45 {
                        tok!(2, sk::MINUS_MINUS);
                    }
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::MINUS_EQUALS);
                    }
                    tok!(1, sk::MINUS);
                }
                46 => {
                    if is_digit(self.cc(self.pos + 1)) {
                        self.scan_number();
                        self.token = sk::NUMERIC_LITERAL;
                        return self.token;
                    }
                    if self.cc(self.pos + 1) == 46 && self.cc(self.pos + 2) == 46 {
                        tok!(3, sk::DOT_DOT_DOT);
                    }
                    tok!(1, sk::DOT);
                }
                47 => {
                    if self.cc(self.pos + 1) == 47 {
                        self.pos += 2;
                        while self.pos < self.end() && !is_line_break(self.cc(self.pos)) {
                            self.pos += 1;
                        }
                        continue;
                    }
                    if self.cc(self.pos + 1) == 42 {
                        self.pos += 2;
                        let mut closed = false;
                        while self.pos < self.end() {
                            let c = self.cc(self.pos);
                            if c == 42 && self.cc(self.pos + 1) == 47 {
                                self.pos += 2;
                                closed = true;
                                break;
                            }
                            self.pos += 1;
                            if is_line_break(c) {
                                self.token_flags |= PRECEDING_LINE_BREAK;
                            }
                        }
                        if !closed {
                            self.error();
                        }
                        continue;
                    }
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::SLASH_EQUALS);
                    }
                    tok!(1, sk::SLASH);
                }
                48 => {
                    if self.pos + 2 < self.end() && (self.cc(self.pos + 1) == 88 || self.cc(self.pos + 1) == 120) {
                        self.pos += 2;
                        let mut digits = self.scan_hex_digits(1, true, true);
                        if digits.is_empty() {
                            self.error();
                            digits = vec![48];
                        }
                        let mut value: Vec<u16> = "0x".encode_utf16().collect();
                        value.extend(digits);
                        self.token_value = value;
                        self.token_flags |= HEX_SPECIFIER;
                        self.token = self.check_big_int_suffix();
                        return self.token;
                    } else if self.pos + 2 < self.end() && (self.cc(self.pos + 1) == 66 || self.cc(self.pos + 1) == 98) {
                        self.pos += 2;
                        let mut digits = self.scan_binary_or_octal_digits(2);
                        if digits.is_empty() {
                            self.error();
                            digits = vec![48];
                        }
                        let mut value: Vec<u16> = "0b".encode_utf16().collect();
                        value.extend(digits);
                        self.token_value = value;
                        self.token_flags |= BINARY_SPECIFIER;
                        self.token = self.check_big_int_suffix();
                        return self.token;
                    } else if self.pos + 2 < self.end() && (self.cc(self.pos + 1) == 79 || self.cc(self.pos + 1) == 111) {
                        self.pos += 2;
                        let mut digits = self.scan_binary_or_octal_digits(8);
                        if digits.is_empty() {
                            self.error();
                            digits = vec![48];
                        }
                        let mut value: Vec<u16> = "0o".encode_utf16().collect();
                        value.extend(digits);
                        self.token_value = value;
                        self.token_flags |= OCTAL_SPECIFIER;
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
                58 => tok!(1, sk::COLON),
                59 => tok!(1, sk::SEMICOLON),
                60 => {
                    if self.is_conflict_marker_trivia(self.pos) {
                        self.pos = self.scan_conflict_marker_trivia(self.pos);
                        continue;
                    }
                    if self.cc(self.pos + 1) == 60 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::LESS_THAN_LESS_THAN_EQUALS);
                        }
                        tok!(2, sk::LESS_THAN_LESS_THAN);
                    }
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::LESS_THAN_EQUALS);
                    }
                    if self.cc(self.pos + 1) == 47 && self.cc(self.pos + 2) != 42 {
                        tok!(2, sk::LESS_THAN_SLASH);
                    }
                    tok!(1, sk::LESS_THAN);
                }
                61 => {
                    if self.is_conflict_marker_trivia(self.pos) {
                        self.pos = self.scan_conflict_marker_trivia(self.pos);
                        continue;
                    }
                    if self.cc(self.pos + 1) == 61 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::EQUALS_EQUALS_EQUALS);
                        }
                        tok!(2, sk::EQUALS_EQUALS);
                    }
                    if self.cc(self.pos + 1) == 62 {
                        tok!(2, sk::EQUALS_GREATER_THAN);
                    }
                    tok!(1, sk::EQUALS);
                }
                62 => {
                    if self.is_conflict_marker_trivia(self.pos) {
                        self.pos = self.scan_conflict_marker_trivia(self.pos);
                        continue;
                    }
                    tok!(1, sk::GREATER_THAN);
                }
                63 => {
                    if self.cc(self.pos + 1) == 46 && !is_digit(self.cc(self.pos + 2)) {
                        tok!(2, sk::QUESTION_DOT);
                    }
                    if self.cc(self.pos + 1) == 63 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::QUESTION_QUESTION_EQUALS);
                        }
                        tok!(2, sk::QUESTION_QUESTION);
                    }
                    tok!(1, sk::QUESTION);
                }
                91 => tok!(1, sk::OPEN_BRACKET),
                93 => tok!(1, sk::CLOSE_BRACKET),
                94 => {
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::CARET_EQUALS);
                    }
                    tok!(1, sk::CARET);
                }
                123 => tok!(1, sk::OPEN_BRACE),
                124 => {
                    if self.is_conflict_marker_trivia(self.pos) {
                        self.pos = self.scan_conflict_marker_trivia(self.pos);
                        continue;
                    }
                    if self.cc(self.pos + 1) == 124 {
                        if self.cc(self.pos + 2) == 61 {
                            tok!(3, sk::BAR_BAR_EQUALS);
                        }
                        tok!(2, sk::BAR_BAR);
                    }
                    if self.cc(self.pos + 1) == 61 {
                        tok!(2, sk::BAR_EQUALS);
                    }
                    tok!(1, sk::BAR);
                }
                125 => tok!(1, sk::CLOSE_BRACE),
                126 => tok!(1, sk::TILDE),
                64 => tok!(1, sk::AT),
                92 => {
                    let extended = self.peek_extended_unicode_escape();
                    if extended >= 0 && is_identifier_start(extended.min(i64::from(i32::MAX)) as i32) {
                        let mut value = Vec::new();
                        self.scan_extended_unicode_escape(true, &mut value);
                        let rest = self.scan_identifier_parts();
                        value.extend(rest);
                        self.token_value = value;
                        return self.get_identifier_token();
                    }
                    let cooked = self.peek_unicode_escape();
                    if cooked >= 0 && is_identifier_start(cooked as i32) {
                        self.pos += 6;
                        self.token_flags |= UNICODE_ESCAPE;
                        let mut value = Vec::new();
                        push_code_point(&mut value, cooked as u32);
                        let rest = self.scan_identifier_parts();
                        value.extend(rest);
                        self.token_value = value;
                        return self.get_identifier_token();
                    }
                    self.error();
                    tok!(1, sk::UNKNOWN);
                }
                35 => {
                    if self.pos != 0 && self.cc(self.pos + 1) == 33 {
                        self.error();
                        tok!(1, sk::UNKNOWN);
                    }
                    let char_after_hash = self.cp(self.pos + 1);
                    if char_after_hash == 92 {
                        self.pos += 1;
                        let extended = self.peek_extended_unicode_escape();
                        if extended >= 0 && is_identifier_start(extended.min(i64::from(i32::MAX)) as i32) {
                            let mut value: Vec<u16> = vec![35];
                            self.scan_extended_unicode_escape(true, &mut value);
                            let rest = self.scan_identifier_parts();
                            value.extend(rest);
                            self.token_value = value;
                            self.token = sk::PRIVATE_IDENTIFIER;
                            return self.token;
                        }
                        let cooked = self.peek_unicode_escape();
                        if cooked >= 0 && is_identifier_start(cooked as i32) {
                            self.pos += 6;
                            self.token_flags |= UNICODE_ESCAPE;
                            let mut value: Vec<u16> = vec![35];
                            push_code_point(&mut value, cooked as u32);
                            let rest = self.scan_identifier_parts();
                            value.extend(rest);
                            self.token_value = value;
                            self.token = sk::PRIVATE_IDENTIFIER;
                            return self.token;
                        }
                        self.pos -= 1;
                    }
                    if is_identifier_start(char_after_hash) {
                        self.pos += 1;
                        self.scan_identifier(char_after_hash);
                    } else {
                        self.token_value = vec![35];
                        self.error();
                        self.pos += char_size(ch);
                    }
                    self.token = sk::PRIVATE_IDENTIFIER;
                    return self.token;
                }
                65533 => {
                    self.error();
                    self.pos = self.end();
                    self.token = sk::NON_TEXT_FILE_MARKER_TRIVIA;
                    return self.token;
                }
                _ => {
                    if let Some(kind) = self.scan_identifier(ch) {
                        self.token = kind;
                        return self.token;
                    } else if is_white_space_single_line(ch) {
                        self.pos += char_size(ch);
                        continue;
                    } else if is_line_break(ch) {
                        self.token_flags |= PRECEDING_LINE_BREAK;
                        self.pos += char_size(ch);
                        continue;
                    }
                    self.error();
                    self.pos += char_size(ch);
                    self.token = sk::UNKNOWN;
                    return self.token;
                }
            }
        }
    }

    // ----- scanner: rescans -----

    fn re_scan_greater_token(&mut self) -> K {
        if self.token == sk::GREATER_THAN {
            if self.cc(self.pos) == 62 {
                if self.cc(self.pos + 1) == 62 {
                    if self.cc(self.pos + 2) == 61 {
                        self.pos += 3;
                        self.token = sk::GREATER_THAN_GREATER_THAN_GREATER_THAN_EQUALS;
                        return self.token;
                    }
                    self.pos += 2;
                    self.token = sk::GREATER_THAN_GREATER_THAN_GREATER_THAN;
                    return self.token;
                }
                if self.cc(self.pos + 1) == 61 {
                    self.pos += 2;
                    self.token = sk::GREATER_THAN_GREATER_THAN_EQUALS;
                    return self.token;
                }
                self.pos += 1;
                self.token = sk::GREATER_THAN_GREATER_THAN;
                return self.token;
            }
            if self.cc(self.pos) == 61 {
                self.pos += 1;
                self.token = sk::GREATER_THAN_EQUALS;
                return self.token;
            }
        }
        self.token
    }

    fn re_scan_asterisk_equals_token(&mut self) -> K {
        self.pos = self.token_start + 1;
        self.token = sk::EQUALS;
        self.token
    }

    /// `reScanSlashToken`: a regular expression literal (its flags too); the body is not validated.
    fn re_scan_slash_token(&mut self) -> K {
        if self.token == sk::SLASH || self.token == sk::SLASH_EQUALS {
            let start_of_body = self.token_start + 1;
            self.pos = start_of_body;
            let mut in_escape = false;
            let mut in_character_class = false;
            loop {
                let ch = self.cc(self.pos);
                if ch == -1 || is_line_break(ch) {
                    self.token_flags |= UNTERMINATED;
                    break;
                }
                if in_escape {
                    in_escape = false;
                } else if ch == 47 && !in_character_class {
                    break;
                } else if ch == 91 {
                    in_character_class = true;
                } else if ch == 92 {
                    in_escape = true;
                } else if ch == 93 {
                    in_character_class = false;
                }
                self.pos += 1;
            }
            let end_of_body = self.pos;
            if self.token_flags & UNTERMINATED != 0 {
                self.pos = start_of_body;
                in_escape = false;
                let mut character_class_depth = 0;
                let mut in_decimal_quantifier = false;
                let mut group_depth = 0;
                while self.pos < end_of_body {
                    let ch = self.cc(self.pos);
                    if in_escape {
                        in_escape = false;
                    } else if ch == 92 {
                        in_escape = true;
                    } else if ch == 91 {
                        character_class_depth += 1;
                    } else if ch == 93 && character_class_depth != 0 {
                        character_class_depth -= 1;
                    } else if character_class_depth == 0 {
                        if ch == 123 {
                            in_decimal_quantifier = true;
                        } else if ch == 125 && in_decimal_quantifier {
                            in_decimal_quantifier = false;
                        } else if !in_decimal_quantifier {
                            if ch == 40 {
                                group_depth += 1;
                            } else if ch == 41 && group_depth != 0 {
                                group_depth -= 1;
                            } else if ch == 41 || ch == 93 || ch == 125 {
                                break;
                            }
                        }
                    }
                    self.pos += 1;
                }
                while is_white_space_like(self.cc_before(self.pos)) || self.cc_before(self.pos) == 59 {
                    self.pos -= 1;
                }
                self.error();
            } else {
                self.pos += 1;
                loop {
                    let ch = self.cp(self.pos);
                    if ch == -1 || !is_identifier_part(ch) {
                        break;
                    }
                    self.pos += char_size(ch);
                }
            }
            self.token_value = self.substring(self.token_start, self.pos).to_vec();
            self.token = sk::REGULAR_EXPRESSION_LITERAL;
        }
        self.token
    }

    fn re_scan_template_token(&mut self, is_tagged_template: bool) -> K {
        self.pos = self.token_start;
        self.token = self.scan_template_and_set_token_value(!is_tagged_template);
        self.token
    }

    fn re_scan_less_than_token(&mut self) -> K {
        if self.token == sk::LESS_THAN_LESS_THAN {
            self.pos = self.token_start + 1;
            self.token = sk::LESS_THAN;
            return self.token;
        }
        self.token
    }

    fn re_scan_question_token(&mut self) -> K {
        self.pos = self.token_start + 1;
        self.token = sk::QUESTION;
        self.token
    }

    fn re_scan_jsx_token(&mut self) -> K {
        self.pos = self.full_start;
        self.token_start = self.full_start;
        self.scan_jsx_token(true)
    }

    fn scan_jsx_token(&mut self, allow_multiline_jsx_text: bool) -> K {
        self.full_start = self.pos;
        self.token_start = self.pos;
        if self.pos >= self.end() {
            self.token = sk::END_OF_FILE;
            return self.token;
        }
        let mut ch = self.cc(self.pos);
        if ch == 60 {
            if self.cc(self.pos + 1) == 47 {
                self.pos += 2;
                self.token = sk::LESS_THAN_SLASH;
                return self.token;
            }
            self.pos += 1;
            self.token = sk::LESS_THAN;
            return self.token;
        }
        if ch == 123 {
            self.pos += 1;
            self.token = sk::OPEN_BRACE;
            return self.token;
        }
        let mut first_non_whitespace: i64 = 0;
        while self.pos < self.end() {
            ch = self.cc(self.pos);
            if ch == 123 {
                break;
            }
            if ch == 60 {
                if self.is_conflict_marker_trivia(self.pos) {
                    self.pos = self.scan_conflict_marker_trivia(self.pos);
                    self.token = sk::CONFLICT_MARKER_TRIVIA;
                    return self.token;
                }
                break;
            }
            if ch == 62 || ch == 125 {
                self.error();
            }
            if is_line_break(ch) && first_non_whitespace == 0 {
                first_non_whitespace = -1;
            } else if !allow_multiline_jsx_text && is_line_break(ch) && first_non_whitespace > 0 {
                break;
            } else if !is_white_space_like(ch) {
                first_non_whitespace = self.pos as i64;
            }
            self.pos += 1;
        }
        self.token_value = self.substring(self.full_start, self.pos).to_vec();
        // like TypeScript, the text kinds are returned without becoming the scanner's `token`
        if first_non_whitespace == -1 { sk::JSX_TEXT_ALL_WHITE_SPACES } else { sk::JSX_TEXT }
    }

    fn scan_jsx_identifier(&mut self) -> K {
        if token_is_identifier_or_keyword(self.token) {
            while self.pos < self.end() {
                let ch = self.cc(self.pos);
                if ch == 45 {
                    self.token_value.push(45);
                    self.pos += 1;
                    continue;
                }
                let old_pos = self.pos;
                let parts = self.scan_identifier_parts();
                self.token_value.extend(parts);
                if self.pos == old_pos {
                    break;
                }
            }
            return self.get_identifier_token();
        }
        self.token
    }

    fn scan_jsx_attribute_value(&mut self) -> K {
        self.full_start = self.pos;
        match self.cc(self.pos) {
            34 | 39 => {
                self.token_value = self.scan_string(true);
                self.token = sk::STRING_LITERAL;
                self.token
            }
            _ => self.scan(),
        }
    }

    /// `scanJsDocToken`
    fn scan_js_doc_token(&mut self) -> K {
        self.full_start = self.pos;
        self.token_start = self.pos;
        self.token_flags = 0;
        if self.pos >= self.end() {
            self.token = sk::END_OF_FILE;
            return self.token;
        }
        let ch = self.cp(self.pos);
        self.pos += char_size(ch);
        let kind = match ch {
            9 | 11 | 12 | 32 => {
                while self.pos < self.end() && is_white_space_single_line(self.cc(self.pos)) {
                    self.pos += 1;
                }
                Some(sk::WHITESPACE_TRIVIA)
            }
            64 => Some(sk::AT),
            13 | 10 => {
                if ch == 13 && self.cc(self.pos) == 10 {
                    self.pos += 1;
                }
                self.token_flags |= PRECEDING_LINE_BREAK;
                Some(sk::NEW_LINE_TRIVIA)
            }
            42 => Some(sk::ASTERISK),
            123 => Some(sk::OPEN_BRACE),
            125 => Some(sk::CLOSE_BRACE),
            91 => Some(sk::OPEN_BRACKET),
            93 => Some(sk::CLOSE_BRACKET),
            40 => Some(sk::OPEN_PAREN),
            41 => Some(sk::CLOSE_PAREN),
            60 => Some(sk::LESS_THAN),
            62 => Some(sk::GREATER_THAN),
            61 => Some(sk::EQUALS),
            44 => Some(sk::COMMA),
            46 => Some(sk::DOT),
            96 => Some(sk::BACKTICK),
            35 => Some(sk::HASH),
            92 => {
                self.pos -= 1;
                let extended = self.peek_extended_unicode_escape();
                if extended >= 0 && is_identifier_start(extended.min(i64::from(i32::MAX)) as i32) {
                    let mut value = Vec::new();
                    self.scan_extended_unicode_escape(true, &mut value);
                    let rest = self.scan_identifier_parts();
                    value.extend(rest);
                    self.token_value = value;
                    return self.get_identifier_token();
                }
                let cooked = self.peek_unicode_escape();
                if cooked >= 0 && is_identifier_start(cooked as i32) {
                    self.pos += 6;
                    self.token_flags |= UNICODE_ESCAPE;
                    let mut value = Vec::new();
                    push_code_point(&mut value, cooked as u32);
                    let rest = self.scan_identifier_parts();
                    value.extend(rest);
                    self.token_value = value;
                    return self.get_identifier_token();
                }
                self.pos += 1;
                Some(sk::UNKNOWN)
            }
            _ => None,
        };
        if let Some(kind) = kind {
            self.token = kind;
            return self.token;
        }
        if is_identifier_start(ch) {
            let mut c = ch;
            // TypeScript loops forever on a trailing `-` at the end of the text; stop at the end instead
            while self.pos < self.end()
                && ({
                    c = self.cp(self.pos);
                    is_identifier_part(c)
                } || c == 45)
            {
                self.pos += char_size(c);
            }
            self.token_value = self.substring(self.token_start, self.pos).to_vec();
            if c == 92 {
                let rest = self.scan_identifier_parts();
                self.token_value.extend(rest);
            }
            self.get_identifier_token()
        } else {
            self.token = sk::UNKNOWN;
            self.token
        }
    }
}

/// `"" + +text` for the text of a decimal number, as a string of units.
fn js_number_text(text: &[u16]) -> Vec<u16> {
    crate::js::number_to_string(js_number_value(text)).encode_utf16().collect()
}

/// `+text` for decimal number text made of digits, a dot, an exponent.
fn js_number_value(text: &[u16]) -> f64 {
    String::from_utf16_lossy(text).parse::<f64>().unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------------------------------
// The parser: infrastructure
// ---------------------------------------------------------------------------------------------------

/// `SignatureFlags`
const SIG_YIELD: u32 = 1;
const SIG_AWAIT: u32 = 2;
const SIG_TYPE: u32 = 4;
const SIG_IGNORE_MISSING_OPEN_BRACE: u32 = 16;
const SIG_JSDOC: u32 = 32;

fn is_modifier_kind(token: K) -> bool {
    matches!(
        token,
        sk::ABSTRACT
            | sk::ACCESSOR
            | sk::ASYNC
            | sk::CONST
            | sk::DECLARE
            | sk::DEFAULT
            | sk::EXPORT
            | sk::IN
            | sk::PUBLIC
            | sk::PRIVATE
            | sk::PROTECTED
            | sk::READONLY
            | sk::STATIC
            | sk::OUT
            | sk::OVERRIDE
    )
}

fn is_parameter_property_modifier(kind: K) -> bool {
    matches!(kind, sk::PUBLIC | sk::PRIVATE | sk::PROTECTED | sk::READONLY | sk::OVERRIDE)
}

fn is_class_member_modifier(kind: K) -> bool {
    is_parameter_property_modifier(kind) || kind == sk::STATIC || kind == sk::OVERRIDE || kind == sk::ACCESSOR
}

fn is_assignment_operator(token: K) -> bool {
    (sk::EQUALS..=sk::CARET_EQUALS).contains(&token)
}

/// `getBinaryOperatorPrecedence`: -1 for a token that is not a binary operator.
fn binary_operator_precedence(kind: K) -> i32 {
    match kind {
        sk::QUESTION_QUESTION | sk::BAR_BAR => 5,
        sk::AMPERSAND_AMPERSAND => 6,
        sk::BAR => 7,
        sk::CARET => 8,
        sk::AMPERSAND => 9,
        sk::EQUALS_EQUALS | sk::EXCLAMATION_EQUALS | sk::EQUALS_EQUALS_EQUALS | sk::EXCLAMATION_EQUALS_EQUALS => 10,
        sk::LESS_THAN
        | sk::GREATER_THAN
        | sk::LESS_THAN_EQUALS
        | sk::GREATER_THAN_EQUALS
        | sk::INSTANCE_OF
        | sk::IN
        | sk::AS
        | sk::SATISFIES => 11,
        sk::LESS_THAN_LESS_THAN | sk::GREATER_THAN_GREATER_THAN | sk::GREATER_THAN_GREATER_THAN_GREATER_THAN => 12,
        sk::PLUS | sk::MINUS => 13,
        sk::ASTERISK | sk::SLASH | sk::PERCENT => 14,
        sk::ASTERISK_ASTERISK => 15,
        _ => -1,
    }
}

/// `isLeftHandSideExpressionKind`
fn is_left_hand_side_expression_kind(kind: K) -> bool {
    matches!(
        kind,
        sk::N_PROPERTY_ACCESS_EXPRESSION
            | sk::N_ELEMENT_ACCESS_EXPRESSION
            | sk::N_NEW_EXPRESSION
            | sk::N_CALL_EXPRESSION
            | sk::N_JSX_ELEMENT
            | sk::N_JSX_SELF_CLOSING_ELEMENT
            | sk::N_JSX_FRAGMENT
            | sk::N_TAGGED_TEMPLATE_EXPRESSION
            | sk::N_ARRAY_LITERAL_EXPRESSION
            | sk::N_PARENTHESIZED_EXPRESSION
            | sk::N_OBJECT_LITERAL_EXPRESSION
            | sk::N_CLASS_EXPRESSION
            | sk::N_FUNCTION_EXPRESSION
            | sk::IDENTIFIER
            | sk::PRIVATE_IDENTIFIER
            | sk::REGULAR_EXPRESSION_LITERAL
            | sk::NUMERIC_LITERAL
            | sk::BIG_INT_LITERAL
            | sk::STRING_LITERAL
            | sk::NO_SUBSTITUTION_TEMPLATE_LITERAL
            | sk::N_TEMPLATE_EXPRESSION
            | sk::FALSE
            | sk::NULL
            | sk::THIS
            | sk::TRUE
            | sk::SUPER
            | sk::N_NON_NULL_EXPRESSION
            | sk::N_META_PROPERTY
            | sk::IMPORT
            | sk::N_MISSING_DECLARATION
    )
}

/// `tagNamesAreEquivalent`
fn tag_names_are_equivalent(a: &Tag, b: &Tag) -> bool {
    a == b
}

impl Parser<'_> {
    // ----- tokens -----

    fn tok(&self) -> K {
        self.current
    }

    fn has_preceding_line_break(&self) -> bool {
        self.token_flags & PRECEDING_LINE_BREAK != 0
    }

    fn next_token_without_check(&mut self) -> K {
        self.current = self.scan();
        self.current
    }

    fn next_token(&mut self) -> K {
        if is_keyword(self.current) && self.token_flags & (UNICODE_ESCAPE | EXTENDED_UNICODE_ESCAPE) != 0 {
            self.error();
        }
        self.next_token_without_check()
    }

    fn next_token_js_doc(&mut self) -> K {
        self.current = self.scan_js_doc_token();
        self.current
    }

    fn rescan_greater_token(&mut self) -> K {
        self.current = self.re_scan_greater_token();
        self.current
    }

    fn rescan_slash_token(&mut self) -> K {
        self.current = self.re_scan_slash_token();
        self.current
    }

    fn rescan_template_token(&mut self, is_tagged_template: bool) -> K {
        self.current = self.re_scan_template_token(is_tagged_template);
        self.current
    }

    fn rescan_less_than_token(&mut self) -> K {
        self.current = self.re_scan_less_than_token();
        self.current
    }

    fn parser_scan_jsx_identifier(&mut self) -> K {
        self.current = self.scan_jsx_identifier();
        self.current
    }

    fn scan_jsx_text(&mut self) -> K {
        self.current = self.scan_jsx_token(true);
        self.current
    }

    fn parser_scan_jsx_attribute_value(&mut self) -> K {
        self.current = self.scan_jsx_attribute_value();
        self.current
    }

    // ----- speculation and contexts -----

    fn save(&self) -> Snapshot {
        Snapshot {
            pos: self.pos,
            full_start: self.full_start,
            token_start: self.token_start,
            token: self.token,
            token_value: self.token_value.clone(),
            token_flags: self.token_flags,
            current: self.current,
            errors: self.errors,
        }
    }

    fn restore(&mut self, s: Snapshot) {
        if self.abort.is_some() {
            return;
        }
        self.pos = s.pos;
        self.full_start = s.full_start;
        self.token_start = s.token_start;
        self.token = s.token;
        self.token_value = s.token_value;
        self.token_flags = s.token_flags;
        self.current = s.current;
        self.errors = s.errors;
    }

    /// `lookAhead`: run `f`, then go back.
    fn look_ahead<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let s = self.save();
        let r = f(self);
        self.restore(s);
        r
    }

    /// `tryParse` for a callback returning a node or undefined: go back when it returns undefined.
    fn try_parse<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let s = self.save();
        let r = f(self);
        if r.is_none() {
            self.restore(s);
        }
        r
    }

    /// `tryParse` for a callback returning a boolean: go back when it returns false.
    fn try_parse_bool(&mut self, f: impl FnOnce(&mut Self) -> bool) -> bool {
        let s = self.save();
        let r = f(self);
        if !r {
            self.restore(s);
        }
        r
    }

    /// Stops the parse the way an exception does: nothing more is read.
    fn throw(&mut self, message: &'static str) {
        if self.abort.is_none() {
            self.abort = Some(message);
        }
        self.pos = self.end();
        self.full_start = self.pos;
        self.token_start = self.pos;
        self.token = sk::END_OF_FILE;
        self.current = sk::END_OF_FILE;
    }

    /// Counts nesting; past `MAX_DEPTH` the parse stops like TypeScript's stack overflow.
    fn guard<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.depth += 1;
        if self.depth > self.max_depth || self.abort.is_some() {
            self.throw("Maximum call stack size exceeded");
        }
        let r = f(self);
        self.depth -= 1;
        r
    }

    fn do_outside_of_context<T>(&mut self, context: u32, f: impl FnOnce(&mut Self) -> T) -> T {
        let to_clear = context & self.context_flags;
        if to_clear != 0 {
            self.context_flags &= !to_clear;
            let r = f(self);
            self.context_flags |= to_clear;
            r
        } else {
            f(self)
        }
    }

    fn do_inside_of_context<T>(&mut self, context: u32, f: impl FnOnce(&mut Self) -> T) -> T {
        let to_set = context & !self.context_flags;
        if to_set != 0 {
            self.context_flags |= to_set;
            let r = f(self);
            self.context_flags &= !to_set;
            r
        } else {
            f(self)
        }
    }

    fn set_context_flag(&mut self, value: bool, flag: u32) {
        if value {
            self.context_flags |= flag;
        } else {
            self.context_flags &= !flag;
        }
    }

    fn allow_in_and<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.do_outside_of_context(CTX_DISALLOW_IN, f)
    }

    fn disallow_in_and<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.do_inside_of_context(CTX_DISALLOW_IN, f)
    }

    fn allow_conditional_types_and<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.do_outside_of_context(CTX_DISALLOW_CONDITIONAL_TYPES, f)
    }

    fn disallow_conditional_types_and<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.do_inside_of_context(CTX_DISALLOW_CONDITIONAL_TYPES, f)
    }

    fn in_yield_context(&self) -> bool {
        self.context_flags & CTX_YIELD != 0
    }

    fn in_await_context(&self) -> bool {
        self.context_flags & CTX_AWAIT != 0
    }

    fn in_decorator_context(&self) -> bool {
        self.context_flags & CTX_DECORATOR != 0
    }

    fn in_disallow_in_context(&self) -> bool {
        self.context_flags & CTX_DISALLOW_IN != 0
    }

    fn in_disallow_conditional_types_context(&self) -> bool {
        self.context_flags & CTX_DISALLOW_CONDITIONAL_TYPES != 0
    }

    // ----- identifiers and expectations -----

    fn is_binding_identifier(&self) -> bool {
        self.tok() == sk::IDENTIFIER || self.tok() > sk::WITH
    }

    fn is_identifier(&self) -> bool {
        if self.tok() == sk::IDENTIFIER {
            return true;
        }
        if self.tok() == sk::YIELD && self.in_yield_context() {
            return false;
        }
        if self.tok() == sk::AWAIT && self.in_await_context() {
            return false;
        }
        self.tok() > sk::WITH
    }

    fn parse_expected(&mut self, kind: K) -> bool {
        self.parse_expected_advance(kind, true)
    }

    fn parse_expected_advance(&mut self, kind: K, should_advance: bool) -> bool {
        if self.tok() == kind {
            if should_advance {
                self.next_token();
            }
            return true;
        }
        self.error();
        false
    }

    /// `parseErrorForMissingSemicolonAfter`: an error unless the expression is the identifier `declare`.
    fn parse_error_for_missing_semicolon_after(&mut self, node: &Node) {
        if let (sk::IDENTIFIER, Data::Text(text)) = (node.kind, &node.data)
            && text.iter().copied().eq("declare".encode_utf16())
        {
            return;
        }
        self.error();
    }

    fn parse_semicolon_after_property_name(&mut self, name: &Node, has_type: bool, has_initializer: bool) {
        if self.tok() == sk::AT && !self.has_preceding_line_break() {
            self.error();
            return;
        }
        if self.tok() == sk::OPEN_PAREN {
            self.error();
            self.next_token();
            return;
        }
        if has_type && !self.can_parse_semicolon() {
            self.error();
            return;
        }
        if self.try_parse_semicolon() {
            return;
        }
        if has_initializer {
            self.error();
            return;
        }
        self.parse_error_for_missing_semicolon_after(name);
    }

    fn parse_expected_matching_brackets(&mut self, close_kind: K) {
        if self.tok() == close_kind {
            self.next_token();
            return;
        }
        self.error();
    }

    fn parse_optional(&mut self, t: K) -> bool {
        if self.tok() == t {
            self.next_token();
            return true;
        }
        false
    }

    /// `parseOptionalToken(t)`: true when the token was there (and is consumed).
    fn parse_optional_token(&mut self, t: K) -> bool {
        if self.tok() == t {
            self.parse_token_node();
            return true;
        }
        false
    }

    /// `parseExpectedToken(t)`: true when the token was there, otherwise a missing node is reported.
    fn parse_expected_token(&mut self, t: K) -> bool {
        if self.parse_optional_token(t) {
            return true;
        }
        self.error();
        false
    }

    fn node(&self, kind: K, pos: usize) -> Node {
        Node { kind, pos, end: self.full_start, op: 0, data: Data::None }
    }

    fn node_with(&self, kind: K, pos: usize, data: Data) -> Node {
        Node { kind, pos, end: self.full_start, op: 0, data }
    }

    fn parse_token_node(&mut self) -> Node {
        let pos = self.full_start;
        let kind = self.tok();
        self.next_token();
        self.node(kind, pos)
    }

    fn can_parse_semicolon(&self) -> bool {
        if self.tok() == sk::SEMICOLON {
            return true;
        }
        self.tok() == sk::CLOSE_BRACE || self.tok() == sk::END_OF_FILE || self.has_preceding_line_break()
    }

    fn try_parse_semicolon(&mut self) -> bool {
        if !self.can_parse_semicolon() {
            return false;
        }
        if self.tok() == sk::SEMICOLON {
            self.next_token();
        }
        true
    }

    fn parse_semicolon(&mut self) -> bool {
        self.try_parse_semicolon() || self.parse_expected(sk::SEMICOLON)
    }

    /// `createMissingNode`: an error is reported when there is a message or it is positioned explicitly.
    fn create_missing_node(&mut self, kind: K, report_at_current_position: bool, has_message: bool) -> Node {
        if report_at_current_position || has_message {
            self.error();
        }
        let pos = self.full_start;
        let data = if kind == sk::IDENTIFIER { Data::Text(Vec::new()) } else { Data::None };
        self.node_with(kind, pos, data)
    }

    fn create_identifier(&mut self, is_identifier: bool) -> Node {
        if is_identifier {
            let pos = if self.skip_jsdoc_leading_asterisks != 0 && self.token_flags & PRECEDING_JSDOC_LEADING_ASTERISKS != 0 {
                self.token_start
            } else {
                self.full_start
            };
            let text = self.token_value.clone();
            self.next_token_without_check();
            return self.node_with(sk::IDENTIFIER, pos, Data::Text(text));
        }
        if self.tok() == sk::PRIVATE_IDENTIFIER {
            self.error();
            return self.create_identifier(true);
        }
        // `Unknown` tokens go through reScanInvalidIdentifier, which can't succeed here: the same
        // identifier tables are used by the first scan.
        let report_at_current_position = self.tok() == sk::END_OF_FILE;
        self.create_missing_node(sk::IDENTIFIER, report_at_current_position, true)
    }

    fn parse_binding_identifier(&mut self) -> Node {
        let is = self.is_binding_identifier();
        self.create_identifier(is)
    }

    fn parse_identifier(&mut self) -> Node {
        let is = self.is_identifier();
        self.create_identifier(is)
    }

    fn parse_identifier_name(&mut self) -> Node {
        let is = token_is_identifier_or_keyword(self.tok());
        self.create_identifier(is)
    }

    fn parse_identifier_name_error_on_unicode_escape_sequence(&mut self) -> Node {
        if self.token_flags & (UNICODE_ESCAPE | EXTENDED_UNICODE_ESCAPE) != 0 {
            self.error();
        }
        let is = token_is_identifier_or_keyword(self.tok());
        self.create_identifier(is)
    }

    fn is_literal_property_name(&self) -> bool {
        token_is_identifier_or_keyword(self.tok())
            || self.tok() == sk::STRING_LITERAL
            || self.tok() == sk::NUMERIC_LITERAL
            || self.tok() == sk::BIG_INT_LITERAL
    }

    fn is_import_attribute_name(&self) -> bool {
        token_is_identifier_or_keyword(self.tok()) || self.tok() == sk::STRING_LITERAL
    }

    fn parse_property_name_worker(&mut self, allow_computed_property_names: bool) -> Node {
        if self.tok() == sk::STRING_LITERAL || self.tok() == sk::NUMERIC_LITERAL || self.tok() == sk::BIG_INT_LITERAL {
            return self.parse_literal_node();
        }
        if allow_computed_property_names && self.tok() == sk::OPEN_BRACKET {
            return self.parse_computed_property_name();
        }
        if self.tok() == sk::PRIVATE_IDENTIFIER {
            return self.parse_private_identifier();
        }
        self.parse_identifier_name()
    }

    fn parse_property_name(&mut self) -> Node {
        self.parse_property_name_worker(true)
    }

    fn parse_computed_property_name(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::OPEN_BRACKET);
        let expression = self.allow_in_and(Self::parse_expression);
        self.parse_expected(sk::CLOSE_BRACKET);
        self.node_with(sk::N_COMPUTED_PROPERTY_NAME, pos, Data::Child(Box::new(expression)))
    }

    fn parse_private_identifier(&mut self) -> Node {
        let pos = self.full_start;
        let text = self.token_value.clone();
        self.next_token();
        self.node_with(sk::PRIVATE_IDENTIFIER, pos, Data::Text(text))
    }

    // ----- modifiers -----

    fn parse_contextual_modifier(&mut self, t: K) -> bool {
        self.tok() == t && self.try_parse_bool(Self::next_token_can_follow_modifier)
    }

    fn next_token_is_on_same_line_and_can_follow_modifier(&mut self) -> bool {
        self.next_token();
        if self.has_preceding_line_break() {
            return false;
        }
        self.can_follow_modifier()
    }

    fn next_token_can_follow_modifier(&mut self) -> bool {
        match self.tok() {
            sk::CONST => self.next_token() == sk::ENUM,
            sk::EXPORT => {
                self.next_token();
                if self.tok() == sk::DEFAULT {
                    return self.look_ahead(Self::next_token_can_follow_default_keyword);
                }
                if self.tok() == sk::TYPE {
                    return self.look_ahead(Self::next_token_can_follow_export_modifier);
                }
                self.can_follow_export_modifier()
            }
            sk::DEFAULT => self.next_token_can_follow_default_keyword(),
            sk::STATIC => {
                self.next_token();
                self.can_follow_modifier()
            }
            sk::GET | sk::SET => {
                self.next_token();
                self.can_follow_get_or_set_keyword()
            }
            _ => self.next_token_is_on_same_line_and_can_follow_modifier(),
        }
    }

    fn can_follow_export_modifier(&self) -> bool {
        self.tok() == sk::AT
            || (self.tok() != sk::ASTERISK && self.tok() != sk::AS && self.tok() != sk::OPEN_BRACE && self.can_follow_modifier())
    }

    fn next_token_can_follow_export_modifier(&mut self) -> bool {
        self.next_token();
        self.can_follow_export_modifier()
    }

    fn parse_any_contextual_modifier(&mut self) -> bool {
        is_modifier_kind(self.tok()) && self.try_parse_bool(Self::next_token_can_follow_modifier)
    }

    fn can_follow_modifier(&self) -> bool {
        self.tok() == sk::OPEN_BRACKET
            || self.tok() == sk::OPEN_BRACE
            || self.tok() == sk::ASTERISK
            || self.tok() == sk::DOT_DOT_DOT
            || self.is_literal_property_name()
    }

    fn can_follow_get_or_set_keyword(&self) -> bool {
        self.tok() == sk::OPEN_BRACKET || self.is_literal_property_name()
    }

    fn next_token_can_follow_default_keyword(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::CLASS
            || self.tok() == sk::FUNCTION
            || self.tok() == sk::INTERFACE
            || self.tok() == sk::AT
            || (self.tok() == sk::ABSTRACT && self.look_ahead(Self::next_token_is_class_keyword_on_same_line))
            || (self.tok() == sk::ASYNC && self.look_ahead(Self::next_token_is_function_keyword_on_same_line))
    }

    // ----- lists -----

    fn is_list_element(&mut self, kind: u32, in_error_recovery: bool) -> bool {
        match kind {
            pc::SOURCE_ELEMENTS | pc::BLOCK_STATEMENTS | pc::SWITCH_CLAUSE_STATEMENTS => {
                !(self.tok() == sk::SEMICOLON && in_error_recovery) && self.is_start_of_statement()
            }
            pc::SWITCH_CLAUSES => self.tok() == sk::CASE || self.tok() == sk::DEFAULT,
            pc::TYPE_MEMBERS => self.look_ahead(Self::is_type_member_start),
            pc::CLASS_MEMBERS => self.look_ahead(Self::is_class_member_start) || (self.tok() == sk::SEMICOLON && !in_error_recovery),
            pc::ENUM_MEMBERS => self.tok() == sk::OPEN_BRACKET || self.is_literal_property_name(),
            pc::OBJECT_LITERAL_MEMBERS => match self.tok() {
                sk::OPEN_BRACKET | sk::ASTERISK | sk::DOT_DOT_DOT | sk::DOT => true,
                _ => self.is_literal_property_name(),
            },
            pc::REST_PROPERTIES => self.is_literal_property_name(),
            pc::OBJECT_BINDING_ELEMENTS => {
                self.tok() == sk::OPEN_BRACKET || self.tok() == sk::DOT_DOT_DOT || self.is_literal_property_name()
            }
            pc::IMPORT_ATTRIBUTES => self.is_import_attribute_name(),
            pc::HERITAGE_CLAUSE_ELEMENT => {
                if self.tok() == sk::OPEN_BRACE {
                    return self.look_ahead(Self::is_valid_heritage_clause_object_literal);
                }
                if !in_error_recovery {
                    self.is_start_of_left_hand_side_expression() && !self.is_heritage_clause_extends_or_implements_keyword()
                } else {
                    self.is_identifier() && !self.is_heritage_clause_extends_or_implements_keyword()
                }
            }
            pc::VARIABLE_DECLARATIONS => self.is_binding_identifier_or_private_identifier_or_pattern(),
            pc::ARRAY_BINDING_ELEMENTS => {
                self.tok() == sk::COMMA || self.tok() == sk::DOT_DOT_DOT || self.is_binding_identifier_or_private_identifier_or_pattern()
            }
            pc::TYPE_PARAMETERS => self.tok() == sk::IN || self.tok() == sk::CONST || self.is_identifier(),
            pc::ARRAY_LITERAL_MEMBERS => {
                if self.tok() == sk::COMMA || self.tok() == sk::DOT {
                    return true;
                }
                self.tok() == sk::DOT_DOT_DOT || self.is_start_of_expression()
            }
            pc::ARGUMENT_EXPRESSIONS => self.tok() == sk::DOT_DOT_DOT || self.is_start_of_expression(),
            pc::PARAMETERS => self.is_start_of_parameter(false),
            pc::JSDOC_PARAMETERS => self.is_start_of_parameter(true),
            pc::TYPE_ARGUMENTS | pc::TUPLE_ELEMENT_TYPES => self.tok() == sk::COMMA || self.is_start_of_type(false),
            pc::HERITAGE_CLAUSES => self.is_heritage_clause(),
            pc::IMPORT_OR_EXPORT_SPECIFIERS => {
                if self.tok() == sk::FROM && self.look_ahead(Self::next_token_is_string_literal) {
                    return false;
                }
                if self.tok() == sk::STRING_LITERAL {
                    return true;
                }
                token_is_identifier_or_keyword(self.tok())
            }
            pc::JSX_ATTRIBUTES => token_is_identifier_or_keyword(self.tok()) || self.tok() == sk::OPEN_BRACE,
            _ => true,
        }
    }

    fn is_valid_heritage_clause_object_literal(&mut self) -> bool {
        if self.next_token() == sk::CLOSE_BRACE {
            let next = self.next_token();
            return next == sk::COMMA || next == sk::OPEN_BRACE || next == sk::EXTENDS || next == sk::IMPLEMENTS;
        }
        true
    }

    fn next_token_is_identifier(&mut self) -> bool {
        self.next_token();
        self.is_identifier()
    }

    fn next_token_is_identifier_or_keyword(&mut self) -> bool {
        self.next_token();
        token_is_identifier_or_keyword(self.tok())
    }

    fn next_token_is_identifier_or_keyword_or_greater_than(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::GREATER_THAN || token_is_identifier_or_keyword(self.tok())
    }

    fn is_heritage_clause_extends_or_implements_keyword(&mut self) -> bool {
        if self.tok() == sk::IMPLEMENTS || self.tok() == sk::EXTENDS {
            return self.look_ahead(Self::next_token_is_start_of_expression);
        }
        false
    }

    fn next_token_is_start_of_expression(&mut self) -> bool {
        self.next_token();
        self.is_start_of_expression()
    }

    fn next_token_is_start_of_type(&mut self) -> bool {
        self.next_token();
        self.is_start_of_type(false)
    }

    fn is_list_terminator(&mut self, kind: u32) -> bool {
        if self.tok() == sk::END_OF_FILE {
            return true;
        }
        match kind {
            pc::BLOCK_STATEMENTS
            | pc::SWITCH_CLAUSES
            | pc::TYPE_MEMBERS
            | pc::CLASS_MEMBERS
            | pc::ENUM_MEMBERS
            | pc::OBJECT_LITERAL_MEMBERS
            | pc::OBJECT_BINDING_ELEMENTS
            | pc::IMPORT_OR_EXPORT_SPECIFIERS
            | pc::IMPORT_ATTRIBUTES => self.tok() == sk::CLOSE_BRACE,
            pc::SWITCH_CLAUSE_STATEMENTS => self.tok() == sk::CLOSE_BRACE || self.tok() == sk::CASE || self.tok() == sk::DEFAULT,
            pc::HERITAGE_CLAUSE_ELEMENT => self.tok() == sk::OPEN_BRACE || self.tok() == sk::EXTENDS || self.tok() == sk::IMPLEMENTS,
            pc::VARIABLE_DECLARATIONS => self.is_variable_declarator_list_terminator(),
            pc::TYPE_PARAMETERS => {
                self.tok() == sk::GREATER_THAN
                    || self.tok() == sk::OPEN_PAREN
                    || self.tok() == sk::OPEN_BRACE
                    || self.tok() == sk::EXTENDS
                    || self.tok() == sk::IMPLEMENTS
            }
            pc::ARGUMENT_EXPRESSIONS => self.tok() == sk::CLOSE_PAREN || self.tok() == sk::SEMICOLON,
            pc::ARRAY_LITERAL_MEMBERS | pc::TUPLE_ELEMENT_TYPES | pc::ARRAY_BINDING_ELEMENTS => self.tok() == sk::CLOSE_BRACKET,
            pc::JSDOC_PARAMETERS | pc::PARAMETERS | pc::REST_PROPERTIES => self.tok() == sk::CLOSE_PAREN || self.tok() == sk::CLOSE_BRACKET,
            pc::TYPE_ARGUMENTS => self.tok() != sk::COMMA,
            pc::HERITAGE_CLAUSES => self.tok() == sk::OPEN_BRACE || self.tok() == sk::CLOSE_BRACE,
            pc::JSX_ATTRIBUTES => self.tok() == sk::GREATER_THAN || self.tok() == sk::SLASH,
            pc::JSX_CHILDREN => self.tok() == sk::LESS_THAN && self.look_ahead(Self::next_token_is_slash),
            _ => false,
        }
    }

    fn is_variable_declarator_list_terminator(&mut self) -> bool {
        if self.can_parse_semicolon() {
            return true;
        }
        if self.tok() == sk::IN || self.tok() == sk::OF {
            return true;
        }
        self.tok() == sk::EQUALS_GREATER_THAN
    }

    fn is_in_some_parsing_context(&mut self) -> bool {
        for kind in 0..pc::COUNT {
            if self.parsing_context & (1 << kind) != 0 && (self.is_list_element(kind, true) || self.is_list_terminator(kind)) {
                return true;
            }
        }
        false
    }

    fn parse_list(&mut self, kind: u32, mut parse_element: impl FnMut(&mut Self)) {
        let save_parsing_context = self.parsing_context;
        self.parsing_context |= 1 << kind;
        while !self.is_list_terminator(kind) {
            if self.is_list_element(kind, false) {
                parse_element(self);
                continue;
            }
            if self.abort_parsing_list_or_move_to_next_token(kind) {
                break;
            }
        }
        self.parsing_context = save_parsing_context;
    }

    fn abort_parsing_list_or_move_to_next_token(&mut self, _kind: u32) -> bool {
        self.error();
        if self.is_in_some_parsing_context() {
            return true;
        }
        self.next_token();
        false
    }

    /// `parseDelimitedList`: None when an element parser gives up (an `undefined` element).
    fn parse_delimited_list<T>(
        &mut self,
        kind: u32,
        mut parse_element: impl FnMut(&mut Self) -> Option<T>,
        consider_semicolon_as_delimiter: bool,
    ) -> Option<Vec<T>> {
        let save_parsing_context = self.parsing_context;
        self.parsing_context |= 1 << kind;
        let mut list = Vec::new();
        loop {
            if self.is_list_element(kind, false) {
                let start_pos = self.full_start;
                let Some(result) = parse_element(self) else {
                    self.parsing_context = save_parsing_context;
                    return None;
                };
                list.push(result);
                if self.parse_optional(sk::COMMA) {
                    continue;
                }
                if self.is_list_terminator(kind) {
                    break;
                }
                self.parse_expected(sk::COMMA);
                if consider_semicolon_as_delimiter && self.tok() == sk::SEMICOLON && !self.has_preceding_line_break() {
                    self.next_token();
                }
                if start_pos == self.full_start {
                    self.next_token();
                }
                continue;
            }
            if self.is_list_terminator(kind) {
                break;
            }
            if self.abort_parsing_list_or_move_to_next_token(kind) {
                break;
            }
        }
        self.parsing_context = save_parsing_context;
        Some(list)
    }

    /// `parseBracketedList`: the elements are not needed by any caller.
    fn parse_bracketed_list<T>(&mut self, kind: u32, parse_element: impl FnMut(&mut Self) -> Option<T>, open: K, close: K) {
        if self.parse_expected(open) {
            self.parse_delimited_list(kind, parse_element, false);
            self.parse_expected(close);
        }
    }

    fn parse_entity_name(&mut self, allow_reserved_words: bool) -> Node {
        let pos = self.full_start;
        let mut entity = if allow_reserved_words { self.parse_identifier_name() } else { self.parse_identifier() };
        while self.parse_optional(sk::DOT) {
            if self.tok() == sk::LESS_THAN {
                break;
            }
            self.parse_right_side_of_dot(allow_reserved_words, false, true);
            entity = self.node(sk::N_QUALIFIED_NAME, pos);
        }
        entity
    }

    fn parse_right_side_of_dot(
        &mut self,
        allow_identifier_names: bool,
        allow_private_identifiers: bool,
        allow_unicode_escape_sequence_in_identifier_name: bool,
    ) -> Node {
        if self.has_preceding_line_break() && token_is_identifier_or_keyword(self.tok()) {
            let matches_pattern = self.look_ahead(Self::next_token_is_identifier_or_keyword_on_same_line);
            if matches_pattern {
                return self.create_missing_node(sk::IDENTIFIER, true, true);
            }
        }
        if self.tok() == sk::PRIVATE_IDENTIFIER {
            let node = self.parse_private_identifier();
            return if allow_private_identifiers { node } else { self.create_missing_node(sk::IDENTIFIER, true, true) };
        }
        if allow_identifier_names {
            return if allow_unicode_escape_sequence_in_identifier_name {
                self.parse_identifier_name()
            } else {
                self.parse_identifier_name_error_on_unicode_escape_sequence()
            };
        }
        self.parse_identifier()
    }

    // ----- templates and literals -----

    /// Parses `head span*`; the token is the template head.
    fn parse_template_expression(&mut self, is_tagged_template: bool) -> Node {
        let pos = self.full_start;
        self.parse_template_head(is_tagged_template);
        loop {
            let literal = self.parse_template_span(is_tagged_template);
            if literal != sk::TEMPLATE_MIDDLE {
                break;
            }
        }
        self.node(sk::N_TEMPLATE_EXPRESSION, pos)
    }

    fn parse_template_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_template_head(false);
        loop {
            let literal = self.parse_template_type_span();
            if literal != sk::TEMPLATE_MIDDLE {
                break;
            }
        }
        self.node(N_OTHER, pos)
    }

    fn parse_template_type_span(&mut self) -> K {
        self.parse_type();
        self.parse_literal_of_template_span(false)
    }

    /// The kind of the literal that ends the span.
    fn parse_literal_of_template_span(&mut self, is_tagged_template: bool) -> K {
        if self.tok() == sk::CLOSE_BRACE {
            self.rescan_template_token(is_tagged_template);
            self.parse_template_middle_or_template_tail()
        } else {
            self.parse_expected_token(sk::TEMPLATE_TAIL);
            sk::TEMPLATE_TAIL
        }
    }

    fn parse_template_span(&mut self, is_tagged_template: bool) -> K {
        self.allow_in_and(Self::parse_expression);
        self.parse_literal_of_template_span(is_tagged_template)
    }

    fn parse_literal_node(&mut self) -> Node {
        let kind = self.tok();
        self.parse_literal_like_node(kind)
    }

    fn parse_template_head(&mut self, is_tagged_template: bool) {
        if !is_tagged_template && self.token_flags & IS_INVALID != 0 {
            self.rescan_template_token(false);
        }
        let kind = self.tok();
        self.parse_literal_like_node(kind);
    }

    fn parse_template_middle_or_template_tail(&mut self) -> K {
        let kind = self.tok();
        self.parse_literal_like_node(kind);
        kind
    }

    fn parse_literal_like_node(&mut self, kind: K) -> Node {
        let pos = self.full_start;
        let data =
            if kind == sk::NUMERIC_LITERAL { Data::Num(js_number_value(&self.token_value)) } else { Data::Text(self.token_value.clone()) };
        self.next_token();
        self.node_with(kind, pos, data)
    }
}

// ---------------------------------------------------------------------------------------------------
// The parser: types, parameters, type members
// ---------------------------------------------------------------------------------------------------

/// `typeHasArrowFunctionBlockingParseError`
fn type_has_arrow_function_blocking_parse_error(node: &Node) -> bool {
    match node.kind {
        sk::N_TYPE_REFERENCE => node.op == 1,
        sk::N_FUNCTION_TYPE | sk::N_CONSTRUCTOR_TYPE => {
            node.op == 1 || matches!(&node.data, Data::Child(t) if type_has_arrow_function_blocking_parse_error(t))
        }
        sk::N_PARENTHESIZED_TYPE => {
            matches!(&node.data, Data::Child(t) if type_has_arrow_function_blocking_parse_error(t))
        }
        _ => false,
    }
}

impl Parser<'_> {
    fn parse_entity_name_of_type_reference(&mut self) -> Node {
        self.parse_entity_name(true)
    }

    fn parse_type_arguments_of_type_reference(&mut self) {
        if !self.has_preceding_line_break() && self.rescan_less_than_token() == sk::LESS_THAN {
            self.parse_bracketed_list(pc::TYPE_ARGUMENTS, |p| Some(p.parse_type()), sk::LESS_THAN, sk::GREATER_THAN);
        }
    }

    fn parse_type_reference(&mut self) -> Node {
        let pos = self.full_start;
        let name = self.parse_entity_name_of_type_reference();
        self.parse_type_arguments_of_type_reference();
        let mut node = self.node(sk::N_TYPE_REFERENCE, pos);
        node.op = u16::from(name.is_missing());
        node
    }

    fn parse_this_type_predicate(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.parse_type();
        self.node(N_OTHER, pos)
    }

    fn parse_this_type_node(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.node(N_OTHER, pos)
    }

    fn parse_jsdoc_all_type(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.node(N_OTHER, pos)
    }

    fn parse_jsdoc_non_nullable_type(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.parse_non_array_type();
        self.node(N_OTHER, pos)
    }

    fn parse_jsdoc_unknown_or_nullable_type(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        let t = self.tok();
        if t == sk::COMMA || t == sk::CLOSE_BRACE || t == sk::CLOSE_PAREN || t == sk::GREATER_THAN || t == sk::EQUALS || t == sk::BAR {
            self.node(N_OTHER, pos)
        } else {
            self.parse_type();
            self.node(N_OTHER, pos)
        }
    }

    fn parse_jsdoc_function_type(&mut self) -> Node {
        let pos = self.full_start;
        if self.try_parse_bool(Self::next_token_is_open_paren) {
            self.parse_parameters(SIG_TYPE | SIG_JSDOC);
            self.parse_return_type(sk::COLON, false);
            return self.node(sk::N_JS_DOC_FUNCTION_TYPE, pos);
        }
        self.parse_identifier_name();
        self.node(sk::N_TYPE_REFERENCE, pos)
    }

    fn parse_jsdoc_parameter(&mut self) -> Option<Node> {
        let pos = self.full_start;
        if self.tok() == sk::THIS || self.tok() == sk::NEW {
            self.parse_identifier_name();
            self.parse_expected(sk::COLON);
        }
        self.parse_jsdoc_type();
        Some(self.node(sk::N_PARAMETER, pos))
    }

    fn parse_jsdoc_type(&mut self) -> Node {
        self.skip_jsdoc_leading_asterisks += 1;
        let pos = self.full_start;
        if self.parse_optional(sk::MODULE) {
            loop {
                match self.tok() {
                    sk::CLOSE_BRACE | sk::END_OF_FILE | sk::COMMA | sk::WHITESPACE_TRIVIA => break,
                    _ => {
                        self.next_token_js_doc();
                    }
                }
            }
            self.skip_jsdoc_leading_asterisks -= 1;
            return self.node(N_OTHER, pos);
        }
        self.parse_optional(sk::DOT_DOT_DOT);
        self.parse_type_or_type_predicate();
        self.skip_jsdoc_leading_asterisks -= 1;
        if self.tok() == sk::EQUALS {
            self.next_token();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_type_query(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::TYPE_OF);
        self.parse_entity_name(true);
        if !self.has_preceding_line_break() {
            self.try_parse_type_arguments();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_type_parameter(&mut self) -> Option<Node> {
        let pos = self.full_start;
        self.parse_modifiers(false, true, false);
        self.parse_identifier();
        if self.parse_optional(sk::EXTENDS) {
            if self.is_start_of_type(false) || !self.is_start_of_expression() {
                self.parse_type();
            } else {
                self.parse_unary_expression_or_higher();
            }
        }
        if self.parse_optional(sk::EQUALS) {
            self.parse_type();
        }
        Some(self.node(N_OTHER, pos))
    }

    fn parse_type_parameters(&mut self) {
        if self.tok() == sk::LESS_THAN {
            self.parse_bracketed_list(pc::TYPE_PARAMETERS, Self::parse_type_parameter, sk::LESS_THAN, sk::GREATER_THAN);
        }
    }

    fn is_start_of_parameter(&mut self, is_jsdoc_parameter: bool) -> bool {
        self.tok() == sk::DOT_DOT_DOT
            || self.is_binding_identifier_or_private_identifier_or_pattern()
            || is_modifier_kind(self.tok())
            || self.tok() == sk::AT
            || self.is_start_of_type(!is_jsdoc_parameter)
    }

    fn parse_name_of_parameter(&mut self, modifiers: &Modifiers) -> Node {
        let name = self.parse_identifier_or_pattern();
        if name.is_missing() && !modifiers.some() && is_modifier_kind(self.tok()) {
            self.next_token();
        }
        name
    }

    fn is_parameter_name_start(&self) -> bool {
        self.is_binding_identifier() || self.tok() == sk::OPEN_BRACKET || self.tok() == sk::OPEN_BRACE
    }

    fn parse_parameter(&mut self, in_outer_await_context: bool) -> Option<Node> {
        self.parse_parameter_worker(in_outer_await_context, true)
    }

    fn parse_parameter_for_speculation(&mut self, in_outer_await_context: bool) -> Option<Node> {
        self.parse_parameter_worker(in_outer_await_context, false)
    }

    fn parse_parameter_worker(&mut self, in_outer_await_context: bool, allow_ambiguity: bool) -> Option<Node> {
        let pos = self.full_start;
        let modifiers = if in_outer_await_context {
            self.do_inside_of_context(CTX_AWAIT, |p| p.parse_modifiers(true, false, false))
        } else {
            self.do_outside_of_context(CTX_AWAIT, |p| p.parse_modifiers(true, false, false))
        };
        if self.tok() == sk::THIS {
            self.create_identifier(true);
            self.parse_type_annotation();
            return Some(self.node(sk::N_PARAMETER, pos));
        }
        self.parse_optional_token(sk::DOT_DOT_DOT);
        if !allow_ambiguity && !self.is_parameter_name_start() {
            return None;
        }
        self.parse_name_of_parameter(&modifiers);
        self.parse_optional_token(sk::QUESTION);
        self.parse_type_annotation();
        self.parse_initializer();
        Some(self.node(sk::N_PARAMETER, pos))
    }

    fn parse_return_type(&mut self, return_token: K, is_type: bool) -> Option<Node> {
        if self.should_parse_return_type(return_token, is_type) {
            return Some(self.allow_conditional_types_and(Self::parse_type_or_type_predicate));
        }
        None
    }

    fn should_parse_return_type(&mut self, return_token: K, is_type: bool) -> bool {
        if return_token == sk::EQUALS_GREATER_THAN {
            self.parse_expected(return_token);
            true
        } else if self.parse_optional(sk::COLON) {
            true
        } else if is_type && self.tok() == sk::EQUALS_GREATER_THAN {
            self.error();
            self.next_token();
            true
        } else {
            false
        }
    }

    /// `parseParametersWorker`: None when a parameter parser gives up.
    fn parse_parameters_worker(&mut self, flags: u32, allow_ambiguity: bool) -> Option<Vec<Node>> {
        let saved_yield_context = self.in_yield_context();
        let saved_await_context = self.in_await_context();
        self.set_context_flag(flags & SIG_YIELD != 0, CTX_YIELD);
        self.set_context_flag(flags & SIG_AWAIT != 0, CTX_AWAIT);
        let parameters = if flags & SIG_JSDOC != 0 {
            self.parse_delimited_list(pc::JSDOC_PARAMETERS, Self::parse_jsdoc_parameter, false)
        } else {
            self.parse_delimited_list(
                pc::PARAMETERS,
                |p| {
                    if allow_ambiguity {
                        p.parse_parameter(saved_await_context)
                    } else {
                        p.parse_parameter_for_speculation(saved_await_context)
                    }
                },
                false,
            )
        };
        self.set_context_flag(saved_yield_context, CTX_YIELD);
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        parameters
    }

    /// `parseParameters`: true when the parameter list is missing (no `(`).
    fn parse_parameters(&mut self, flags: u32) -> bool {
        if !self.parse_expected(sk::OPEN_PAREN) {
            return true;
        }
        self.parse_parameters_worker(flags, true);
        self.parse_expected(sk::CLOSE_PAREN);
        false
    }

    fn parse_type_member_semicolon(&mut self) {
        if self.parse_optional(sk::COMMA) {
            return;
        }
        self.parse_semicolon();
    }

    fn parse_signature_member(&mut self, kind: K) -> Option<Node> {
        let pos = self.full_start;
        if kind == sk::N_CONSTRUCTOR {
            self.parse_expected(sk::NEW);
        }
        self.parse_type_parameters();
        self.parse_parameters(SIG_TYPE);
        self.parse_return_type(sk::COLON, true);
        self.parse_type_member_semicolon();
        Some(self.node(N_OTHER, pos))
    }

    fn is_index_signature(&mut self) -> bool {
        self.tok() == sk::OPEN_BRACKET && self.look_ahead(Self::is_unambiguously_index_signature)
    }

    fn is_unambiguously_index_signature(&mut self) -> bool {
        self.next_token();
        if self.tok() == sk::DOT_DOT_DOT || self.tok() == sk::CLOSE_BRACKET {
            return true;
        }
        if is_modifier_kind(self.tok()) {
            self.next_token();
            if self.is_identifier() {
                return true;
            }
        } else if !self.is_identifier() {
            return false;
        } else {
            self.next_token();
        }
        if self.tok() == sk::COLON || self.tok() == sk::COMMA {
            return true;
        }
        if self.tok() != sk::QUESTION {
            return false;
        }
        self.next_token();
        self.tok() == sk::COLON || self.tok() == sk::COMMA || self.tok() == sk::CLOSE_BRACKET
    }

    fn parse_index_signature_declaration(&mut self, pos: usize) -> Option<Node> {
        self.parse_bracketed_list(pc::PARAMETERS, |p| p.parse_parameter(false), sk::OPEN_BRACKET, sk::CLOSE_BRACKET);
        self.parse_type_annotation();
        self.parse_type_member_semicolon();
        Some(self.node(N_OTHER, pos))
    }

    fn parse_property_or_method_signature(&mut self, pos: usize) -> Option<Node> {
        self.parse_property_name();
        self.parse_optional_token(sk::QUESTION);
        if self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN {
            self.parse_type_parameters();
            self.parse_parameters(SIG_TYPE);
            self.parse_return_type(sk::COLON, true);
        } else {
            self.parse_type_annotation();
            if self.tok() == sk::EQUALS {
                self.parse_initializer();
            }
        }
        self.parse_type_member_semicolon();
        Some(self.node(N_OTHER, pos))
    }

    fn is_type_member_start(&mut self) -> bool {
        if self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN || self.tok() == sk::GET || self.tok() == sk::SET {
            return true;
        }
        let mut id_token = false;
        while is_modifier_kind(self.tok()) {
            id_token = true;
            self.next_token();
        }
        if self.tok() == sk::OPEN_BRACKET {
            return true;
        }
        if self.is_literal_property_name() {
            id_token = true;
            self.next_token();
        }
        if id_token {
            return self.tok() == sk::OPEN_PAREN
                || self.tok() == sk::LESS_THAN
                || self.tok() == sk::QUESTION
                || self.tok() == sk::COLON
                || self.tok() == sk::COMMA
                || self.can_parse_semicolon();
        }
        false
    }

    fn parse_type_member(&mut self) -> Option<Node> {
        if self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN {
            return self.parse_signature_member(0);
        }
        if self.tok() == sk::NEW && self.look_ahead(Self::next_token_is_open_paren_or_less_than) {
            return self.parse_signature_member(sk::N_CONSTRUCTOR);
        }
        let pos = self.full_start;
        self.parse_modifiers(false, false, false);
        if self.parse_contextual_modifier(sk::GET) {
            return Some(self.parse_accessor_declaration(pos, sk::N_GET_ACCESSOR, SIG_TYPE));
        }
        if self.parse_contextual_modifier(sk::SET) {
            return Some(self.parse_accessor_declaration(pos, sk::N_SET_ACCESSOR, SIG_TYPE));
        }
        if self.is_index_signature() {
            return self.parse_index_signature_declaration(pos);
        }
        self.parse_property_or_method_signature(pos)
    }

    fn next_token_is_open_paren_or_less_than(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN
    }

    fn next_token_is_dot(&mut self) -> bool {
        self.next_token() == sk::DOT
    }

    fn next_token_is_open_paren_or_less_than_or_dot(&mut self) -> bool {
        matches!(self.next_token(), sk::OPEN_PAREN | sk::LESS_THAN | sk::DOT)
    }

    fn parse_type_literal(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_object_type_members();
        self.node(N_OTHER, pos)
    }

    fn parse_object_type_members(&mut self) {
        if self.parse_expected(sk::OPEN_BRACE) {
            self.parse_list(pc::TYPE_MEMBERS, |p| {
                p.parse_type_member();
            });
            self.parse_expected(sk::CLOSE_BRACE);
        }
    }

    fn is_start_of_mapped_type(&mut self) -> bool {
        self.next_token();
        if self.tok() == sk::PLUS || self.tok() == sk::MINUS {
            return self.next_token() == sk::READONLY;
        }
        if self.tok() == sk::READONLY {
            self.next_token();
        }
        self.tok() == sk::OPEN_BRACKET && self.next_token_is_identifier() && self.next_token() == sk::IN
    }

    fn parse_mapped_type_parameter(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_identifier_name();
        self.parse_expected(sk::IN);
        self.parse_type();
        self.node(N_OTHER, pos)
    }

    fn parse_mapped_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::OPEN_BRACE);
        if self.tok() == sk::READONLY || self.tok() == sk::PLUS || self.tok() == sk::MINUS {
            let readonly_token = self.parse_token_node();
            if readonly_token.kind != sk::READONLY {
                self.parse_expected(sk::READONLY);
            }
        }
        self.parse_expected(sk::OPEN_BRACKET);
        self.parse_mapped_type_parameter();
        if self.parse_optional(sk::AS) {
            self.parse_type();
        }
        self.parse_expected(sk::CLOSE_BRACKET);
        if self.tok() == sk::QUESTION || self.tok() == sk::PLUS || self.tok() == sk::MINUS {
            let question_token = self.parse_token_node();
            if question_token.kind != sk::QUESTION {
                self.parse_expected(sk::QUESTION);
            }
        }
        self.parse_type_annotation();
        self.parse_semicolon();
        self.parse_list(pc::TYPE_MEMBERS, |p| {
            p.parse_type_member();
        });
        self.parse_expected(sk::CLOSE_BRACE);
        self.node(N_OTHER, pos)
    }

    fn parse_tuple_element_type(&mut self) -> Node {
        let pos = self.full_start;
        if self.parse_optional(sk::DOT_DOT_DOT) {
            self.parse_type();
            return self.node(N_OTHER, pos);
        }
        self.parse_type()
    }

    fn is_next_token_colon_or_question_colon(&mut self) -> bool {
        self.next_token() == sk::COLON || (self.tok() == sk::QUESTION && self.next_token() == sk::COLON)
    }

    fn is_tuple_element_name(&mut self) -> bool {
        if self.tok() == sk::DOT_DOT_DOT {
            return token_is_identifier_or_keyword(self.next_token()) && self.is_next_token_colon_or_question_colon();
        }
        token_is_identifier_or_keyword(self.tok()) && self.is_next_token_colon_or_question_colon()
    }

    fn parse_tuple_element_name_or_tuple_element_type(&mut self) -> Option<Node> {
        if self.look_ahead(Self::is_tuple_element_name) {
            let pos = self.full_start;
            self.parse_optional_token(sk::DOT_DOT_DOT);
            self.parse_identifier_name();
            self.parse_optional_token(sk::QUESTION);
            self.parse_expected(sk::COLON);
            self.parse_tuple_element_type();
            return Some(self.node(N_OTHER, pos));
        }
        Some(self.parse_tuple_element_type())
    }

    fn parse_tuple_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_bracketed_list(
            pc::TUPLE_ELEMENT_TYPES,
            Self::parse_tuple_element_name_or_tuple_element_type,
            sk::OPEN_BRACKET,
            sk::CLOSE_BRACKET,
        );
        self.node(N_OTHER, pos)
    }

    fn parse_parenthesized_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::OPEN_PAREN);
        let ty = self.parse_type();
        self.parse_expected(sk::CLOSE_PAREN);
        self.node_with(sk::N_PARENTHESIZED_TYPE, pos, Data::Child(Box::new(ty)))
    }

    fn parse_modifiers_for_constructor_type(&mut self) {
        if self.tok() == sk::ABSTRACT {
            self.next_token();
        }
    }

    fn parse_function_or_constructor_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_modifiers_for_constructor_type();
        let is_constructor_type = self.parse_optional(sk::NEW);
        self.parse_type_parameters();
        let missing_parameters = self.parse_parameters(SIG_TYPE);
        let ty = self.parse_return_type(sk::EQUALS_GREATER_THAN, false);
        let mut node = self.node(if is_constructor_type { sk::N_CONSTRUCTOR_TYPE } else { sk::N_FUNCTION_TYPE }, pos);
        node.op = u16::from(missing_parameters);
        if let Some(ty) = ty {
            node.data = Data::Child(Box::new(ty));
        }
        node
    }

    fn parse_keyword_and_no_dot(&mut self) -> Option<Node> {
        let node = self.parse_token_node();
        if self.tok() == sk::DOT { None } else { Some(node) }
    }

    fn parse_literal_type_node(&mut self, negative: bool) -> Node {
        let pos = self.full_start;
        if negative {
            self.next_token();
        }
        if self.tok() == sk::TRUE || self.tok() == sk::FALSE || self.tok() == sk::NULL {
            self.parse_token_node();
        } else {
            let kind = self.tok();
            self.parse_literal_like_node(kind);
        }
        self.node(N_OTHER, pos)
    }

    fn is_start_of_type_of_import_type(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::IMPORT
    }

    fn parse_import_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_optional(sk::TYPE_OF);
        self.parse_expected(sk::IMPORT);
        self.parse_expected(sk::OPEN_PAREN);
        self.parse_type();
        if self.parse_optional(sk::COMMA) {
            self.parse_expected(sk::OPEN_BRACE);
            let current = self.tok();
            if current == sk::WITH || current == sk::ASSERT {
                self.next_token();
            } else {
                self.error();
            }
            self.parse_expected(sk::COLON);
            self.parse_import_attributes(current, true);
            self.parse_optional(sk::COMMA);
            self.parse_expected(sk::CLOSE_BRACE);
        }
        self.parse_expected(sk::CLOSE_PAREN);
        if self.parse_optional(sk::DOT) {
            self.parse_entity_name_of_type_reference();
        }
        self.parse_type_arguments_of_type_reference();
        self.node(N_OTHER, pos)
    }

    fn next_token_is_numeric_or_big_int_literal(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::NUMERIC_LITERAL || self.tok() == sk::BIG_INT_LITERAL
    }

    fn parse_non_array_type(&mut self) -> Node {
        self.guard(|p| p.parse_non_array_type_worker())
    }

    fn parse_non_array_type_worker(&mut self) -> Node {
        match self.tok() {
            sk::ANY
            | sk::UNKNOWN_KW
            | sk::STRING
            | sk::NUMBER
            | sk::BIG_INT
            | sk::SYMBOL
            | sk::BOOLEAN
            | sk::UNDEFINED
            | sk::NEVER
            | sk::OBJECT => {
                if let Some(node) = self.try_parse(Self::parse_keyword_and_no_dot) {
                    node
                } else {
                    self.parse_type_reference()
                }
            }
            sk::ASTERISK_EQUALS => {
                self.re_scan_asterisk_equals_token();
                self.parse_jsdoc_all_type()
            }
            sk::ASTERISK => self.parse_jsdoc_all_type(),
            sk::QUESTION_QUESTION => {
                self.re_scan_question_token();
                self.parse_jsdoc_unknown_or_nullable_type()
            }
            sk::QUESTION => self.parse_jsdoc_unknown_or_nullable_type(),
            sk::FUNCTION => self.parse_jsdoc_function_type(),
            sk::EXCLAMATION => self.parse_jsdoc_non_nullable_type(),
            sk::NO_SUBSTITUTION_TEMPLATE_LITERAL
            | sk::STRING_LITERAL
            | sk::NUMERIC_LITERAL
            | sk::BIG_INT_LITERAL
            | sk::TRUE
            | sk::FALSE
            | sk::NULL => self.parse_literal_type_node(false),
            sk::MINUS => {
                if self.look_ahead(Self::next_token_is_numeric_or_big_int_literal) {
                    self.parse_literal_type_node(true)
                } else {
                    self.parse_type_reference()
                }
            }
            sk::VOID => self.parse_token_node(),
            sk::THIS => {
                let this_keyword = self.parse_this_type_node();
                if self.tok() == sk::IS && !self.has_preceding_line_break() { self.parse_this_type_predicate() } else { this_keyword }
            }
            sk::TYPE_OF => {
                if self.look_ahead(Self::is_start_of_type_of_import_type) {
                    self.parse_import_type()
                } else {
                    self.parse_type_query()
                }
            }
            sk::OPEN_BRACE => {
                if self.look_ahead(Self::is_start_of_mapped_type) {
                    self.parse_mapped_type()
                } else {
                    self.parse_type_literal()
                }
            }
            sk::OPEN_BRACKET => self.parse_tuple_type(),
            sk::OPEN_PAREN => self.parse_parenthesized_type(),
            sk::IMPORT => self.parse_import_type(),
            sk::ASSERTS => {
                if self.look_ahead(Self::next_token_is_identifier_or_keyword_on_same_line) {
                    self.parse_asserts_type_predicate()
                } else {
                    self.parse_type_reference()
                }
            }
            sk::TEMPLATE_HEAD => self.parse_template_type(),
            _ => self.parse_type_reference(),
        }
    }

    fn is_start_of_type(&mut self, in_start_of_parameter: bool) -> bool {
        match self.tok() {
            sk::ANY
            | sk::UNKNOWN_KW
            | sk::STRING
            | sk::NUMBER
            | sk::BIG_INT
            | sk::BOOLEAN
            | sk::READONLY
            | sk::SYMBOL
            | sk::UNIQUE
            | sk::VOID
            | sk::UNDEFINED
            | sk::NULL
            | sk::THIS
            | sk::TYPE_OF
            | sk::NEVER
            | sk::OPEN_BRACE
            | sk::OPEN_BRACKET
            | sk::LESS_THAN
            | sk::BAR
            | sk::AMPERSAND
            | sk::NEW
            | sk::STRING_LITERAL
            | sk::NUMERIC_LITERAL
            | sk::BIG_INT_LITERAL
            | sk::TRUE
            | sk::FALSE
            | sk::OBJECT
            | sk::ASTERISK
            | sk::QUESTION
            | sk::EXCLAMATION
            | sk::DOT_DOT_DOT
            | sk::INFER
            | sk::IMPORT
            | sk::ASSERTS
            | sk::NO_SUBSTITUTION_TEMPLATE_LITERAL
            | sk::TEMPLATE_HEAD => true,
            sk::FUNCTION => !in_start_of_parameter,
            sk::MINUS => !in_start_of_parameter && self.look_ahead(Self::next_token_is_numeric_or_big_int_literal),
            sk::OPEN_PAREN => !in_start_of_parameter && self.look_ahead(Self::is_start_of_parenthesized_or_function_type),
            _ => self.is_identifier(),
        }
    }

    fn is_start_of_parenthesized_or_function_type(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::CLOSE_PAREN || self.is_start_of_parameter(false) || self.is_start_of_type(false)
    }

    fn parse_postfix_type_or_higher(&mut self) -> Node {
        let pos = self.full_start;
        let mut ty = self.parse_non_array_type();
        while !self.has_preceding_line_break() {
            match self.tok() {
                sk::EXCLAMATION => {
                    self.next_token();
                    ty = self.node(N_OTHER, pos);
                }
                sk::QUESTION => {
                    if self.look_ahead(Self::next_token_is_start_of_type) {
                        return ty;
                    }
                    self.next_token();
                    ty = self.node(N_OTHER, pos);
                }
                sk::OPEN_BRACKET => {
                    self.parse_expected(sk::OPEN_BRACKET);
                    if self.is_start_of_type(false) {
                        self.parse_type();
                        self.parse_expected(sk::CLOSE_BRACKET);
                    } else {
                        self.parse_expected(sk::CLOSE_BRACKET);
                    }
                    ty = self.node(N_OTHER, pos);
                }
                _ => return ty,
            }
        }
        ty
    }

    fn parse_type_operator(&mut self, operator: K) -> Node {
        let pos = self.full_start;
        self.parse_expected(operator);
        self.parse_type_operator_or_higher();
        self.node(N_OTHER, pos)
    }

    fn try_parse_constraint_of_infer_type(&mut self) -> Option<Node> {
        if self.parse_optional(sk::EXTENDS) {
            let constraint = self.disallow_conditional_types_and(Self::parse_type);
            if self.in_disallow_conditional_types_context() || self.tok() != sk::QUESTION {
                return Some(constraint);
            }
        }
        None
    }

    fn parse_type_parameter_of_infer_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_identifier();
        self.try_parse(Self::try_parse_constraint_of_infer_type);
        self.node(N_OTHER, pos)
    }

    fn parse_infer_type(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::INFER);
        self.parse_type_parameter_of_infer_type();
        self.node(N_OTHER, pos)
    }

    fn parse_type_operator_or_higher(&mut self) -> Node {
        self.guard(|p| {
            let operator = p.tok();
            match operator {
                sk::KEY_OF | sk::UNIQUE | sk::READONLY => p.parse_type_operator(operator),
                sk::INFER => p.parse_infer_type(),
                _ => p.allow_conditional_types_and(Self::parse_postfix_type_or_higher),
            }
        })
    }

    fn parse_function_or_constructor_type_to_error(&mut self) -> Option<Node> {
        if self.is_start_of_function_type_or_constructor_type() {
            let ty = self.parse_function_or_constructor_type();
            // must be parenthesized inside a union or intersection type
            self.error();
            return Some(ty);
        }
        None
    }

    fn parse_union_or_intersection_type(&mut self, operator: K, parse_constituent_type: fn(&mut Self) -> Node) -> Node {
        let pos = self.full_start;
        let has_leading_operator = self.parse_optional(operator);
        let mut ty = if has_leading_operator { self.parse_function_or_constructor_type_to_error() } else { None }
            .unwrap_or_else(|| parse_constituent_type(self));
        if self.tok() == operator || has_leading_operator {
            while self.parse_optional(operator) {
                if self.parse_function_or_constructor_type_to_error().is_none() {
                    parse_constituent_type(self);
                }
            }
            ty = self.node(N_OTHER, pos);
        }
        ty
    }

    fn parse_intersection_type_or_higher(&mut self) -> Node {
        self.parse_union_or_intersection_type(sk::AMPERSAND, Self::parse_type_operator_or_higher)
    }

    fn parse_union_type_or_higher(&mut self) -> Node {
        self.parse_union_or_intersection_type(sk::BAR, Self::parse_intersection_type_or_higher)
    }

    fn next_token_is_new_keyword(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::NEW
    }

    fn is_start_of_function_type_or_constructor_type(&mut self) -> bool {
        if self.tok() == sk::LESS_THAN {
            return true;
        }
        if self.tok() == sk::OPEN_PAREN && self.look_ahead(Self::is_unambiguously_start_of_function_type) {
            return true;
        }
        self.tok() == sk::NEW || (self.tok() == sk::ABSTRACT && self.look_ahead(Self::next_token_is_new_keyword))
    }

    fn skip_parameter_start(&mut self) -> bool {
        if is_modifier_kind(self.tok()) {
            self.parse_modifiers(false, false, false);
        }
        if self.is_identifier() || self.tok() == sk::THIS {
            self.next_token();
            return true;
        }
        if self.tok() == sk::OPEN_BRACKET || self.tok() == sk::OPEN_BRACE {
            let previous_error_count = self.errors;
            self.parse_identifier_or_pattern();
            return previous_error_count == self.errors;
        }
        false
    }

    fn is_unambiguously_start_of_function_type(&mut self) -> bool {
        self.next_token();
        if self.tok() == sk::CLOSE_PAREN || self.tok() == sk::DOT_DOT_DOT {
            return true;
        }
        if self.skip_parameter_start() {
            if self.tok() == sk::COLON || self.tok() == sk::COMMA || self.tok() == sk::QUESTION || self.tok() == sk::EQUALS {
                return true;
            }
            if self.tok() == sk::CLOSE_PAREN {
                self.next_token();
                if self.tok() == sk::EQUALS_GREATER_THAN {
                    return true;
                }
            }
        }
        false
    }

    fn parse_type_or_type_predicate(&mut self) -> Node {
        let pos = self.full_start;
        let type_predicate_variable = self.is_identifier() && self.try_parse(Self::parse_type_predicate_prefix).is_some();
        let ty = self.parse_type();
        if type_predicate_variable { self.node(N_OTHER, pos) } else { ty }
    }

    fn parse_type_predicate_prefix(&mut self) -> Option<Node> {
        let id = self.parse_identifier();
        if self.tok() == sk::IS && !self.has_preceding_line_break() {
            self.next_token();
            return Some(id);
        }
        None
    }

    fn parse_asserts_type_predicate(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected_token(sk::ASSERTS);
        if self.tok() == sk::THIS {
            self.parse_this_type_node();
        } else {
            self.parse_identifier();
        }
        if self.parse_optional(sk::IS) {
            self.parse_type();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_type(&mut self) -> Node {
        self.guard(|p| p.parse_type_worker())
    }

    fn parse_type_worker(&mut self) -> Node {
        if self.context_flags & CTX_TYPE_EXCLUDES != 0 {
            return self.do_outside_of_context(CTX_TYPE_EXCLUDES, Self::parse_type);
        }
        if self.is_start_of_function_type_or_constructor_type() {
            return self.parse_function_or_constructor_type();
        }
        let pos = self.full_start;
        let ty = self.parse_union_type_or_higher();
        if !self.in_disallow_conditional_types_context() && !self.has_preceding_line_break() && self.parse_optional(sk::EXTENDS) {
            self.disallow_conditional_types_and(Self::parse_type);
            self.parse_expected(sk::QUESTION);
            self.allow_conditional_types_and(Self::parse_type);
            self.parse_expected(sk::COLON);
            self.allow_conditional_types_and(Self::parse_type);
            return self.node(N_OTHER, pos);
        }
        ty
    }

    fn parse_type_annotation(&mut self) -> Option<Node> {
        if self.parse_optional(sk::COLON) { Some(self.parse_type()) } else { None }
    }

    fn try_parse_type_arguments(&mut self) {
        if self.tok() == sk::LESS_THAN {
            self.parse_bracketed_list(pc::TYPE_ARGUMENTS, |p| Some(p.parse_type()), sk::LESS_THAN, sk::GREATER_THAN);
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// The parser: expressions
// ---------------------------------------------------------------------------------------------------

/// Result of `isParenthesizedArrowFunctionExpression`
#[derive(Clone, Copy, PartialEq)]
enum Tristate {
    False,
    True,
    Unknown,
}

impl Parser<'_> {
    fn is_start_of_left_hand_side_expression(&mut self) -> bool {
        match self.tok() {
            sk::THIS
            | sk::SUPER
            | sk::NULL
            | sk::TRUE
            | sk::FALSE
            | sk::NUMERIC_LITERAL
            | sk::BIG_INT_LITERAL
            | sk::STRING_LITERAL
            | sk::NO_SUBSTITUTION_TEMPLATE_LITERAL
            | sk::TEMPLATE_HEAD
            | sk::OPEN_PAREN
            | sk::OPEN_BRACKET
            | sk::OPEN_BRACE
            | sk::FUNCTION
            | sk::CLASS
            | sk::NEW
            | sk::SLASH
            | sk::SLASH_EQUALS
            | sk::IDENTIFIER => true,
            sk::IMPORT => self.look_ahead(Self::next_token_is_open_paren_or_less_than_or_dot),
            _ => self.is_identifier(),
        }
    }

    fn is_start_of_expression(&mut self) -> bool {
        if self.is_start_of_left_hand_side_expression() {
            return true;
        }
        match self.tok() {
            sk::PLUS
            | sk::MINUS
            | sk::TILDE
            | sk::EXCLAMATION
            | sk::DELETE
            | sk::TYPE_OF
            | sk::VOID
            | sk::PLUS_PLUS
            | sk::MINUS_MINUS
            | sk::LESS_THAN
            | sk::AWAIT
            | sk::YIELD
            | sk::PRIVATE_IDENTIFIER
            | sk::AT => true,
            _ => {
                if self.is_binary_operator() {
                    return true;
                }
                self.is_identifier()
            }
        }
    }

    fn is_start_of_expression_statement(&mut self) -> bool {
        self.tok() != sk::OPEN_BRACE
            && self.tok() != sk::FUNCTION
            && self.tok() != sk::CLASS
            && self.tok() != sk::AT
            && self.is_start_of_expression()
    }

    fn parse_expression(&mut self) -> Node {
        let save_decorator_context = self.in_decorator_context();
        if save_decorator_context {
            self.set_context_flag(false, CTX_DECORATOR);
        }
        let pos = self.full_start;
        let mut expr = self.parse_assignment_expression_or_higher(true);
        while self.parse_optional_token(sk::COMMA) {
            self.parse_assignment_expression_or_higher(true);
            expr = self.node(sk::N_BINARY_EXPRESSION, pos);
        }
        if save_decorator_context {
            self.set_context_flag(true, CTX_DECORATOR);
        }
        expr
    }

    fn parse_initializer(&mut self) -> Option<Node> {
        if self.parse_optional(sk::EQUALS) { Some(self.parse_assignment_expression_or_higher(true)) } else { None }
    }

    fn parse_assignment_expression_or_higher(&mut self, allow_return_type_in_arrow_function: bool) -> Node {
        self.guard(|p| p.parse_assignment_expression_or_higher_worker(allow_return_type_in_arrow_function))
    }

    fn parse_assignment_expression_or_higher_worker(&mut self, allow_return_type_in_arrow_function: bool) -> Node {
        if self.is_yield_expression() {
            return self.parse_yield_expression();
        }
        let arrow_expression = self
            .try_parse_parenthesized_arrow_function_expression(allow_return_type_in_arrow_function)
            .or_else(|| self.try_parse_async_simple_arrow_function_expression(allow_return_type_in_arrow_function));
        if let Some(arrow_expression) = arrow_expression {
            return arrow_expression;
        }
        let pos = self.full_start;
        let expr = self.parse_binary_expression_or_higher(0);
        if expr.kind == sk::IDENTIFIER && self.tok() == sk::EQUALS_GREATER_THAN {
            return self.parse_simple_arrow_function_expression(pos, false, allow_return_type_in_arrow_function);
        }
        if is_left_hand_side_expression_kind(expr.kind) && is_assignment_operator(self.rescan_greater_token()) {
            self.parse_token_node();
            self.parse_assignment_expression_or_higher(allow_return_type_in_arrow_function);
            return self.node(sk::N_BINARY_EXPRESSION, pos);
        }
        self.parse_conditional_expression_rest(expr, pos, allow_return_type_in_arrow_function)
    }

    fn is_yield_expression(&mut self) -> bool {
        if self.tok() == sk::YIELD {
            if self.in_yield_context() {
                return true;
            }
            return self.look_ahead(Self::next_token_is_identifier_or_keyword_or_literal_on_same_line);
        }
        false
    }

    fn parse_yield_expression(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        if !self.has_preceding_line_break() && (self.tok() == sk::ASTERISK || self.is_start_of_expression()) {
            self.parse_optional_token(sk::ASTERISK);
            self.parse_assignment_expression_or_higher(true);
        }
        self.node(N_OTHER, pos)
    }

    fn parse_simple_arrow_function_expression(&mut self, pos: usize, is_async: bool, allow_return_type_in_arrow_function: bool) -> Node {
        self.parse_expected_token(sk::EQUALS_GREATER_THAN);
        self.parse_arrow_function_expression_body(is_async, allow_return_type_in_arrow_function);
        self.node(sk::N_ARROW_FUNCTION, pos)
    }

    fn try_parse_parenthesized_arrow_function_expression(&mut self, allow_return_type_in_arrow_function: bool) -> Option<Node> {
        let tri_state = self.is_parenthesized_arrow_function_expression();
        if tri_state == Tristate::False {
            return None;
        }
        if tri_state == Tristate::True {
            self.parse_parenthesized_arrow_function_expression(true, true)
        } else {
            self.try_parse(|p| p.parse_possible_parenthesized_arrow_function_expression(allow_return_type_in_arrow_function))
        }
    }

    fn is_parenthesized_arrow_function_expression(&mut self) -> Tristate {
        if self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN || self.tok() == sk::ASYNC {
            return self.look_ahead(Self::is_parenthesized_arrow_function_expression_worker);
        }
        if self.tok() == sk::EQUALS_GREATER_THAN {
            return Tristate::True;
        }
        Tristate::False
    }

    fn is_parenthesized_arrow_function_expression_worker(&mut self) -> Tristate {
        if self.tok() == sk::ASYNC {
            self.next_token();
            if self.has_preceding_line_break() {
                return Tristate::False;
            }
            if self.tok() != sk::OPEN_PAREN && self.tok() != sk::LESS_THAN {
                return Tristate::False;
            }
        }
        let first = self.tok();
        let second = self.next_token();
        if first == sk::OPEN_PAREN {
            if second == sk::CLOSE_PAREN {
                let third = self.next_token();
                return match third {
                    sk::EQUALS_GREATER_THAN | sk::COLON | sk::OPEN_BRACE => Tristate::True,
                    _ => Tristate::False,
                };
            }
            if second == sk::OPEN_BRACKET || second == sk::OPEN_BRACE {
                return Tristate::Unknown;
            }
            if second == sk::DOT_DOT_DOT {
                return Tristate::True;
            }
            if is_modifier_kind(second) && second != sk::ASYNC && self.look_ahead(Self::next_token_is_identifier) {
                if self.next_token() == sk::AS {
                    return Tristate::False;
                }
                return Tristate::True;
            }
            if !self.is_identifier() && second != sk::THIS {
                return Tristate::False;
            }
            match self.next_token() {
                sk::COLON => Tristate::True,
                sk::QUESTION => {
                    self.next_token();
                    if self.tok() == sk::COLON || self.tok() == sk::COMMA || self.tok() == sk::EQUALS || self.tok() == sk::CLOSE_PAREN {
                        return Tristate::True;
                    }
                    Tristate::False
                }
                sk::COMMA | sk::EQUALS | sk::CLOSE_PAREN => Tristate::Unknown,
                _ => Tristate::False,
            }
        } else {
            if !self.is_identifier() && self.tok() != sk::CONST {
                return Tristate::False;
            }
            // JSON is parsed with the JSX language variant
            let is_arrow_function_in_jsx = self.look_ahead(|p| {
                p.parse_optional(sk::CONST);
                let third = p.next_token();
                if third == sk::EXTENDS {
                    let fourth = p.next_token();
                    !matches!(fourth, sk::EQUALS | sk::GREATER_THAN | sk::SLASH)
                } else {
                    third == sk::COMMA || third == sk::EQUALS
                }
            });
            if is_arrow_function_in_jsx { Tristate::True } else { Tristate::False }
        }
    }

    fn parse_possible_parenthesized_arrow_function_expression(&mut self, allow_return_type_in_arrow_function: bool) -> Option<Node> {
        let token_pos = self.token_start;
        if self.not_parenthesized_arrow.contains(&token_pos) {
            return None;
        }
        let result = self.parse_parenthesized_arrow_function_expression(false, allow_return_type_in_arrow_function);
        if result.is_none() {
            self.not_parenthesized_arrow.insert(token_pos);
        }
        result
    }

    fn try_parse_async_simple_arrow_function_expression(&mut self, allow_return_type_in_arrow_function: bool) -> Option<Node> {
        if self.tok() == sk::ASYNC && self.look_ahead(Self::is_un_parenthesized_async_arrow_function_worker) == Tristate::True {
            let pos = self.full_start;
            self.parse_modifiers_for_arrow_function();
            self.parse_binary_expression_or_higher(0);
            return Some(self.parse_simple_arrow_function_expression(pos, true, allow_return_type_in_arrow_function));
        }
        None
    }

    fn is_un_parenthesized_async_arrow_function_worker(&mut self) -> Tristate {
        if self.tok() == sk::ASYNC {
            self.next_token();
            if self.has_preceding_line_break() || self.tok() == sk::EQUALS_GREATER_THAN {
                return Tristate::False;
            }
            let expr = self.parse_binary_expression_or_higher(0);
            if !self.has_preceding_line_break() && expr.kind == sk::IDENTIFIER && self.tok() == sk::EQUALS_GREATER_THAN {
                return Tristate::True;
            }
        }
        Tristate::False
    }

    fn parse_parenthesized_arrow_function_expression(
        &mut self,
        allow_ambiguity: bool,
        allow_return_type_in_arrow_function: bool,
    ) -> Option<Node> {
        let pos = self.full_start;
        let modifiers = self.parse_modifiers_for_arrow_function();
        let is_async = if modifiers.has(sk::ASYNC) { SIG_AWAIT } else { 0 };
        self.parse_type_parameters();
        if !self.parse_expected(sk::OPEN_PAREN) {
            if !allow_ambiguity {
                return None;
            }
        } else {
            if !allow_ambiguity {
                self.parse_parameters_worker(is_async, allow_ambiguity)?;
            } else {
                self.parse_parameters_worker(is_async, allow_ambiguity);
            }
            if !self.parse_expected(sk::CLOSE_PAREN) && !allow_ambiguity {
                return None;
            }
        }
        let has_return_colon = self.tok() == sk::COLON;
        let ty = self.parse_return_type(sk::COLON, false);
        if let Some(ty) = &ty
            && !allow_ambiguity
            && type_has_arrow_function_blocking_parse_error(ty)
        {
            return None;
        }
        let mut unwrapped = ty.as_ref();
        while let Some(t) = unwrapped {
            match (&t.data, t.kind) {
                (Data::Child(inner), sk::N_PARENTHESIZED_TYPE) => unwrapped = Some(inner),
                _ => break,
            }
        }
        let has_jsdoc_function_type = unwrapped.is_some_and(|t| t.kind == sk::N_JS_DOC_FUNCTION_TYPE);
        if !allow_ambiguity && self.tok() != sk::EQUALS_GREATER_THAN && (has_jsdoc_function_type || self.tok() != sk::OPEN_BRACE) {
            return None;
        }
        let last_token = self.tok();
        self.parse_expected_token(sk::EQUALS_GREATER_THAN);
        if last_token == sk::EQUALS_GREATER_THAN || last_token == sk::OPEN_BRACE {
            self.parse_arrow_function_expression_body(modifiers.has(sk::ASYNC), allow_return_type_in_arrow_function);
        } else {
            self.parse_identifier();
        }
        if !allow_return_type_in_arrow_function && has_return_colon && self.tok() != sk::COLON {
            return None;
        }
        Some(self.node(sk::N_ARROW_FUNCTION, pos))
    }

    fn parse_arrow_function_expression_body(&mut self, is_async: bool, allow_return_type_in_arrow_function: bool) -> Node {
        if self.tok() == sk::OPEN_BRACE {
            return self.parse_function_block(if is_async { SIG_AWAIT } else { 0 });
        }
        if self.tok() != sk::SEMICOLON
            && self.tok() != sk::FUNCTION
            && self.tok() != sk::CLASS
            && self.is_start_of_statement()
            && !self.is_start_of_expression_statement()
        {
            return self.parse_function_block(SIG_IGNORE_MISSING_OPEN_BRACE | if is_async { SIG_AWAIT } else { 0 });
        }
        let saved_yield_context = self.in_yield_context();
        self.set_context_flag(false, CTX_YIELD);
        let node = if is_async {
            self.do_inside_of_context(CTX_AWAIT, |p| p.parse_assignment_expression_or_higher(allow_return_type_in_arrow_function))
        } else {
            self.do_outside_of_context(CTX_AWAIT, |p| p.parse_assignment_expression_or_higher(allow_return_type_in_arrow_function))
        };
        self.set_context_flag(saved_yield_context, CTX_YIELD);
        node
    }

    fn parse_conditional_expression_rest(&mut self, left_operand: Node, pos: usize, allow_return_type_in_arrow_function: bool) -> Node {
        if !self.parse_optional_token(sk::QUESTION) {
            return left_operand;
        }
        self.do_outside_of_context(CTX_DISALLOW_IN | CTX_DECORATOR, |p| p.parse_assignment_expression_or_higher(false));
        if self.parse_expected_token(sk::COLON) {
            self.parse_assignment_expression_or_higher(allow_return_type_in_arrow_function);
        } else {
            self.create_missing_node(sk::IDENTIFIER, false, true);
        }
        self.node(sk::N_CONDITIONAL_EXPRESSION, pos)
    }

    fn parse_binary_expression_or_higher(&mut self, precedence: i32) -> Node {
        self.guard(|p| {
            let pos = p.full_start;
            let left_operand = p.parse_unary_expression_or_higher();
            p.parse_binary_expression_rest(precedence, left_operand, pos)
        })
    }

    fn parse_binary_expression_rest(&mut self, precedence: i32, mut left_operand: Node, pos: usize) -> Node {
        loop {
            self.rescan_greater_token();
            let new_precedence = binary_operator_precedence(self.tok());
            let consume_current_operator =
                if self.tok() == sk::ASTERISK_ASTERISK { new_precedence >= precedence } else { new_precedence > precedence };
            if !consume_current_operator {
                break;
            }
            if self.tok() == sk::IN && self.in_disallow_in_context() {
                break;
            }
            if self.tok() == sk::AS || self.tok() == sk::SATISFIES {
                if self.has_preceding_line_break() {
                    break;
                }
                self.next_token();
                self.parse_type();
                left_operand = self.node(N_OTHER, left_operand.pos);
            } else {
                self.parse_token_node();
                self.parse_binary_expression_or_higher(new_precedence);
                left_operand = self.node(sk::N_BINARY_EXPRESSION, pos);
            }
        }
        left_operand
    }

    fn is_binary_operator(&self) -> bool {
        if self.in_disallow_in_context() && self.tok() == sk::IN {
            return false;
        }
        binary_operator_precedence(self.tok()) > 0
    }

    fn parse_prefix_unary_expression(&mut self) -> Node {
        let pos = self.full_start;
        let operator = self.tok();
        self.next_token();
        let operand = self.parse_simple_unary_expression();
        let mut node = self.node_with(sk::N_PREFIX_UNARY_EXPRESSION, pos, Data::Child(Box::new(operand)));
        node.op = operator;
        node
    }

    fn parse_delete_expression(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.parse_simple_unary_expression();
        self.node(N_OTHER, pos)
    }

    fn is_await_expression(&mut self) -> bool {
        if self.tok() == sk::AWAIT {
            if self.in_await_context() {
                return true;
            }
            return self.look_ahead(Self::next_token_is_identifier_or_keyword_or_literal_on_same_line);
        }
        false
    }

    fn parse_await_expression(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.parse_simple_unary_expression();
        self.node(N_OTHER, pos)
    }

    fn parse_unary_expression_or_higher(&mut self) -> Node {
        if self.is_update_expression() {
            let pos = self.full_start;
            let update_expression = self.parse_update_expression();
            return if self.tok() == sk::ASTERISK_ASTERISK {
                let precedence = binary_operator_precedence(self.tok());
                self.parse_binary_expression_rest(precedence, update_expression, pos)
            } else {
                update_expression
            };
        }
        let simple_unary_expression = self.parse_simple_unary_expression();
        if self.tok() == sk::ASTERISK_ASTERISK {
            self.error();
        }
        simple_unary_expression
    }

    fn parse_simple_unary_expression(&mut self) -> Node {
        self.guard(|p| p.parse_simple_unary_expression_worker())
    }

    fn parse_simple_unary_expression_worker(&mut self) -> Node {
        match self.tok() {
            sk::PLUS | sk::MINUS | sk::TILDE | sk::EXCLAMATION => self.parse_prefix_unary_expression(),
            sk::DELETE => self.parse_delete_expression(),
            sk::TYPE_OF | sk::VOID => self.parse_delete_expression(),
            sk::LESS_THAN => {
                // the JSX language variant: `<` starts a JSX element
                self.parse_jsx_element_or_self_closing_element_or_fragment(true, true)
            }
            sk::AWAIT if self.is_await_expression() => self.parse_await_expression(),
            _ => self.parse_update_expression(),
        }
    }

    fn is_update_expression(&self) -> bool {
        !matches!(self.tok(), sk::PLUS | sk::MINUS | sk::TILDE | sk::EXCLAMATION | sk::DELETE | sk::TYPE_OF | sk::VOID | sk::AWAIT)
    }

    fn parse_update_expression(&mut self) -> Node {
        if self.tok() == sk::PLUS_PLUS || self.tok() == sk::MINUS_MINUS {
            let pos = self.full_start;
            self.next_token();
            self.parse_left_hand_side_expression_or_higher();
            return self.node(N_OTHER, pos);
        } else if self.tok() == sk::LESS_THAN && self.look_ahead(Self::next_token_is_identifier_or_keyword_or_greater_than) {
            return self.parse_jsx_element_or_self_closing_element_or_fragment(true, false);
        }
        let expression = self.parse_left_hand_side_expression_or_higher();
        if (self.tok() == sk::PLUS_PLUS || self.tok() == sk::MINUS_MINUS) && !self.has_preceding_line_break() {
            self.next_token();
            return self.node(sk::N_POSTFIX_UNARY_EXPRESSION, expression.pos);
        }
        expression
    }

    fn parse_left_hand_side_expression_or_higher(&mut self) -> Node {
        let pos = self.full_start;
        let expression;
        if self.tok() == sk::IMPORT {
            if self.look_ahead(Self::next_token_is_open_paren_or_less_than) {
                expression = self.parse_token_node();
            } else if self.look_ahead(Self::next_token_is_dot) {
                self.next_token();
                self.next_token();
                self.parse_identifier_name();
                expression = self.node(sk::N_META_PROPERTY, pos);
            } else {
                expression = self.parse_member_expression_or_higher();
            }
        } else {
            expression = if self.tok() == sk::SUPER { self.parse_super_expression() } else { self.parse_member_expression_or_higher() };
        }
        self.parse_call_expression_rest(pos, expression)
    }

    fn parse_member_expression_or_higher(&mut self) -> Node {
        let pos = self.full_start;
        let expression = self.parse_primary_expression();
        self.parse_member_expression_rest(pos, expression, true)
    }

    fn parse_super_expression(&mut self) -> Node {
        let pos = self.full_start;
        let expression = self.parse_token_node();
        if self.tok() == sk::OPEN_PAREN || self.tok() == sk::DOT || self.tok() == sk::OPEN_BRACKET {
            return expression;
        }
        self.parse_expected_token(sk::DOT);
        self.parse_right_side_of_dot(true, true, true);
        self.node(sk::N_PROPERTY_ACCESS_EXPRESSION, pos)
    }

    // ----- JSX -----

    fn parse_jsx_element_or_self_closing_element_or_fragment(&mut self, in_expression_context: bool, must_be_unary: bool) -> Node {
        self.guard(|p| p.parse_jsx_element_or_self_closing_element_or_fragment_worker(in_expression_context, must_be_unary))
    }

    fn parse_jsx_element_or_self_closing_element_or_fragment_worker(&mut self, in_expression_context: bool, must_be_unary: bool) -> Node {
        let pos = self.full_start;
        let opening = self.parse_jsx_opening_or_self_closing_element_or_opening_fragment(in_expression_context);
        let result = if opening.kind == sk::N_JSX_OPENING_ELEMENT {
            let opening_tag_name = Self::opening_tag_name(&opening);
            let last_child = self.parse_jsx_children(&opening);
            // a last child that was closed by this element's closing tag takes the closing tag over
            let adopted_closing_tag = match &last_child {
                Some(child) if child.kind == sk::N_JSX_ELEMENT => {
                    let (child_open, child_close) = Self::element_tags(child);
                    (!tag_names_are_equivalent(&child_open, &child_close) && tag_names_are_equivalent(&opening_tag_name, &child_close))
                        .then_some(child_close)
                }
                _ => None,
            };
            let closing_tag_name = match adopted_closing_tag {
                Some(tag) => tag,
                None => {
                    let closing_tag_name = self.parse_jsx_closing_element(&opening_tag_name, in_expression_context);
                    if !tag_names_are_equivalent(&opening_tag_name, &closing_tag_name) {
                        self.error();
                    }
                    closing_tag_name
                }
            };
            self.node_with(sk::N_JSX_ELEMENT, pos, Data::Tags(Box::new((opening_tag_name, closing_tag_name))))
        } else if opening.kind == sk::N_JSX_OPENING_FRAGMENT {
            self.parse_jsx_children(&opening);
            self.parse_jsx_closing_fragment(in_expression_context);
            self.node(sk::N_JSX_FRAGMENT, pos)
        } else {
            opening
        };
        if !must_be_unary && in_expression_context && self.tok() == sk::LESS_THAN {
            // `tryParse` of a callback that always returns a node: never undone
            self.parse_jsx_element_or_self_closing_element_or_fragment(true, false);
            self.error();
            return self.node(sk::N_BINARY_EXPRESSION, pos);
        }
        result
    }

    fn opening_tag_name(opening: &Node) -> Tag {
        match &opening.data {
            Data::Tags(tags) => tags.0.clone(),
            _ => Tag::This,
        }
    }

    fn element_tags(element: &Node) -> (Tag, Tag) {
        match &element.data {
            Data::Tags(tags) => (tags.0.clone(), tags.1.clone()),
            _ => (Tag::This, Tag::This),
        }
    }

    fn parse_jsx_text(&mut self) -> Node {
        let pos = self.full_start;
        self.current = self.scan_jsx_token(true);
        self.node(N_OTHER, pos)
    }

    /// `parseJsxChild`: None for the end of the children.
    fn parse_jsx_child(&mut self, token: K) -> Option<Node> {
        match token {
            sk::END_OF_FILE => {
                self.error();
                None
            }
            sk::LESS_THAN_SLASH | sk::CONFLICT_MARKER_TRIVIA => None,
            sk::JSX_TEXT | sk::JSX_TEXT_ALL_WHITE_SPACES => Some(self.parse_jsx_text()),
            sk::OPEN_BRACE => self.parse_jsx_expression(false),
            sk::LESS_THAN => Some(self.parse_jsx_element_or_self_closing_element_or_fragment(false, false)),
            _ => None,
        }
    }

    /// The last child; `parseJsxChildren`
    fn parse_jsx_children(&mut self, opening_tag: &Node) -> Option<Node> {
        let save_parsing_context = self.parsing_context;
        self.parsing_context |= 1 << pc::JSX_CHILDREN;
        let mut last_child = None;
        loop {
            self.current = self.re_scan_jsx_token();
            let token = self.current;
            let Some(child) = self.parse_jsx_child(token) else {
                break;
            };
            let stop = opening_tag.kind == sk::N_JSX_OPENING_ELEMENT && child.kind == sk::N_JSX_ELEMENT && {
                let (child_open, child_close) = Self::element_tags(&child);
                !tag_names_are_equivalent(&child_open, &child_close)
                    && tag_names_are_equivalent(&Self::opening_tag_name(opening_tag), &child_close)
            };
            last_child = Some(child);
            if stop {
                break;
            }
        }
        self.parsing_context = save_parsing_context;
        last_child
    }

    fn parse_jsx_attributes(&mut self) {
        self.parse_list(pc::JSX_ATTRIBUTES, |p| {
            p.parse_jsx_attribute();
        });
    }

    fn parse_jsx_opening_or_self_closing_element_or_opening_fragment(&mut self, in_expression_context: bool) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::LESS_THAN);
        if self.tok() == sk::GREATER_THAN {
            self.scan_jsx_text();
            return self.node(sk::N_JSX_OPENING_FRAGMENT, pos);
        }
        let tag_name = self.parse_jsx_element_name();
        // type arguments are not parsed in a JavaScript file
        self.parse_jsx_attributes();
        if self.tok() == sk::GREATER_THAN {
            self.scan_jsx_text();
            return self.node_with(sk::N_JSX_OPENING_ELEMENT, pos, Data::Tags(Box::new((tag_name, Tag::This))));
        }
        self.parse_expected(sk::SLASH);
        if self.parse_expected_advance(sk::GREATER_THAN, false) {
            if in_expression_context {
                self.next_token();
            } else {
                self.scan_jsx_text();
            }
        }
        self.node(sk::N_JSX_SELF_CLOSING_ELEMENT, pos)
    }

    fn parse_jsx_element_name(&mut self) -> Tag {
        let initial = self.parse_jsx_tag_name();
        if matches!(initial, Tag::Namespaced(..)) {
            return initial;
        }
        let mut expression = initial;
        while self.parse_optional(sk::DOT) {
            let name = self.parse_right_side_of_dot(true, false, false);
            expression = Tag::Access(Box::new(expression), Self::identifier_text(&name));
        }
        expression
    }

    fn identifier_text(node: &Node) -> Vec<u16> {
        match &node.data {
            Data::Text(text) => text.clone(),
            _ => Vec::new(),
        }
    }

    fn parse_jsx_tag_name(&mut self) -> Tag {
        self.parser_scan_jsx_identifier();
        let is_this = self.tok() == sk::THIS;
        let tag_name = self.parse_identifier_name_error_on_unicode_escape_sequence();
        if self.parse_optional(sk::COLON) {
            self.parser_scan_jsx_identifier();
            let name = self.parse_identifier_name_error_on_unicode_escape_sequence();
            return Tag::Namespaced(Self::identifier_text(&tag_name), Self::identifier_text(&name));
        }
        if is_this { Tag::This } else { Tag::Identifier(Self::identifier_text(&tag_name)) }
    }

    fn parse_jsx_expression(&mut self, in_expression_context: bool) -> Option<Node> {
        let pos = self.full_start;
        if !self.parse_expected(sk::OPEN_BRACE) {
            return None;
        }
        if self.tok() != sk::CLOSE_BRACE {
            if !in_expression_context {
                self.parse_optional_token(sk::DOT_DOT_DOT);
            }
            self.parse_expression();
        }
        if in_expression_context {
            self.parse_expected(sk::CLOSE_BRACE);
        } else if self.parse_expected_advance(sk::CLOSE_BRACE, false) {
            self.scan_jsx_text();
        }
        Some(self.node(N_OTHER, pos))
    }

    fn parse_jsx_attribute(&mut self) {
        if self.tok() == sk::OPEN_BRACE {
            self.parse_jsx_spread_attribute();
            return;
        }
        self.parse_jsx_attribute_name();
        self.parse_jsx_attribute_value();
    }

    fn parse_jsx_attribute_value(&mut self) {
        if self.tok() == sk::EQUALS {
            if self.parser_scan_jsx_attribute_value() == sk::STRING_LITERAL {
                self.parse_literal_node();
                return;
            }
            if self.tok() == sk::OPEN_BRACE {
                self.parse_jsx_expression(true);
                return;
            }
            if self.tok() == sk::LESS_THAN {
                self.parse_jsx_element_or_self_closing_element_or_fragment(true, false);
                return;
            }
            self.error();
        }
    }

    fn parse_jsx_attribute_name(&mut self) {
        self.parser_scan_jsx_identifier();
        self.parse_identifier_name_error_on_unicode_escape_sequence();
        if self.parse_optional(sk::COLON) {
            self.parser_scan_jsx_identifier();
            self.parse_identifier_name_error_on_unicode_escape_sequence();
        }
    }

    fn parse_jsx_spread_attribute(&mut self) {
        self.parse_expected(sk::OPEN_BRACE);
        self.parse_expected(sk::DOT_DOT_DOT);
        self.parse_expression();
        self.parse_expected(sk::CLOSE_BRACE);
    }

    /// `parseJsxClosingElement`: the tag name of the closing element.
    fn parse_jsx_closing_element(&mut self, open_tag: &Tag, in_expression_context: bool) -> Tag {
        self.parse_expected(sk::LESS_THAN_SLASH);
        let tag_name = self.parse_jsx_element_name();
        if self.parse_expected_advance(sk::GREATER_THAN, false) {
            if in_expression_context || !tag_names_are_equivalent(open_tag, &tag_name) {
                self.next_token();
            } else {
                self.scan_jsx_text();
            }
        }
        tag_name
    }

    fn parse_jsx_closing_fragment(&mut self, in_expression_context: bool) {
        self.parse_expected(sk::LESS_THAN_SLASH);
        if self.parse_expected_advance(sk::GREATER_THAN, false) {
            if in_expression_context {
                self.next_token();
            } else {
                self.scan_jsx_text();
            }
        }
    }

    // ----- member and call expressions -----

    fn next_token_is_identifier_or_keyword_or_open_bracket_or_template(&mut self) -> bool {
        self.next_token();
        token_is_identifier_or_keyword(self.tok()) || self.tok() == sk::OPEN_BRACKET || self.is_template_start_of_tagged_template()
    }

    fn is_start_of_optional_property_or_element_access_chain(&mut self) -> bool {
        self.tok() == sk::QUESTION_DOT && self.look_ahead(Self::next_token_is_identifier_or_keyword_or_open_bracket_or_template)
    }

    fn parse_property_access_expression_rest(&mut self, pos: usize) -> Node {
        self.parse_right_side_of_dot(true, true, true);
        self.node(sk::N_PROPERTY_ACCESS_EXPRESSION, pos)
    }

    fn parse_element_access_expression_rest(&mut self, pos: usize) -> Node {
        if self.tok() == sk::CLOSE_BRACKET {
            self.create_missing_node(sk::IDENTIFIER, true, true);
        } else {
            self.allow_in_and(Self::parse_expression);
        }
        self.parse_expected(sk::CLOSE_BRACKET);
        self.node(sk::N_ELEMENT_ACCESS_EXPRESSION, pos)
    }

    fn parse_member_expression_rest(&mut self, pos: usize, mut expression: Node, allow_optional_chain: bool) -> Node {
        loop {
            let mut question_dot_token = false;
            let is_property_access;
            if allow_optional_chain && self.is_start_of_optional_property_or_element_access_chain() {
                question_dot_token = self.parse_expected_token(sk::QUESTION_DOT);
                is_property_access = token_is_identifier_or_keyword(self.tok());
            } else {
                is_property_access = self.parse_optional(sk::DOT);
            }
            if is_property_access {
                expression = self.parse_property_access_expression_rest(pos);
                continue;
            }
            if (question_dot_token || !self.in_decorator_context()) && self.parse_optional(sk::OPEN_BRACKET) {
                expression = self.parse_element_access_expression_rest(pos);
                continue;
            }
            if self.is_template_start_of_tagged_template() {
                expression = self.parse_tagged_template_rest(pos);
                continue;
            }
            if !question_dot_token && self.tok() == sk::EXCLAMATION && !self.has_preceding_line_break() {
                self.next_token();
                expression = self.node(sk::N_NON_NULL_EXPRESSION, pos);
                continue;
            }
            // type arguments are not parsed in expressions in a JavaScript file
            return expression;
        }
    }

    fn is_template_start_of_tagged_template(&self) -> bool {
        self.tok() == sk::NO_SUBSTITUTION_TEMPLATE_LITERAL || self.tok() == sk::TEMPLATE_HEAD
    }

    fn parse_tagged_template_rest(&mut self, pos: usize) -> Node {
        if self.tok() == sk::NO_SUBSTITUTION_TEMPLATE_LITERAL {
            self.rescan_template_token(true);
            self.parse_literal_node();
        } else {
            self.parse_template_expression(true);
        }
        self.node(sk::N_TAGGED_TEMPLATE_EXPRESSION, pos)
    }

    fn parse_call_expression_rest(&mut self, pos: usize, mut expression: Node) -> Node {
        loop {
            expression = self.parse_member_expression_rest(pos, expression, true);
            let question_dot_token = self.parse_optional_token(sk::QUESTION_DOT);
            if question_dot_token && self.is_template_start_of_tagged_template() {
                expression = self.parse_tagged_template_rest(pos);
                continue;
            }
            if self.tok() == sk::OPEN_PAREN {
                self.parse_argument_list();
                expression = self.node(sk::N_CALL_EXPRESSION, pos);
                continue;
            }
            if question_dot_token {
                self.create_missing_node(sk::IDENTIFIER, false, true);
                expression = self.node(sk::N_PROPERTY_ACCESS_EXPRESSION, pos);
            }
            break;
        }
        expression
    }

    fn parse_argument_list(&mut self) {
        self.parse_expected(sk::OPEN_PAREN);
        self.parse_delimited_list(pc::ARGUMENT_EXPRESSIONS, |p| Some(p.parse_argument_expression()), false);
        self.parse_expected(sk::CLOSE_PAREN);
    }

    fn parse_primary_expression(&mut self) -> Node {
        match self.tok() {
            sk::NO_SUBSTITUTION_TEMPLATE_LITERAL => {
                if self.token_flags & IS_INVALID != 0 {
                    self.rescan_template_token(false);
                }
                return self.parse_literal_node();
            }
            sk::NUMERIC_LITERAL | sk::BIG_INT_LITERAL | sk::STRING_LITERAL => {
                return self.parse_literal_node();
            }
            sk::THIS | sk::SUPER | sk::NULL | sk::TRUE | sk::FALSE => {
                return self.parse_token_node();
            }
            sk::OPEN_PAREN => return self.parse_parenthesized_expression(),
            sk::OPEN_BRACKET => return self.parse_array_literal_expression(),
            sk::OPEN_BRACE => return self.parse_object_literal_expression(),
            sk::ASYNC => {
                if self.look_ahead(Self::next_token_is_function_keyword_on_same_line) {
                    return self.parse_function_expression();
                }
            }
            sk::AT => return self.parse_decorated_expression(),
            sk::CLASS => {
                return self.parse_class_declaration_or_expression(&Modifiers::default(), sk::N_CLASS_EXPRESSION);
            }
            sk::FUNCTION => return self.parse_function_expression(),
            sk::NEW => return self.parse_new_expression_or_new_dot_target(),
            sk::SLASH | sk::SLASH_EQUALS => {
                if self.rescan_slash_token() == sk::REGULAR_EXPRESSION_LITERAL {
                    return self.parse_literal_node();
                }
            }
            sk::TEMPLATE_HEAD => return self.parse_template_expression(false),
            sk::PRIVATE_IDENTIFIER => return self.parse_private_identifier(),
            _ => {}
        }
        self.parse_identifier()
    }

    fn parse_parenthesized_expression(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::OPEN_PAREN);
        self.allow_in_and(Self::parse_expression);
        self.parse_expected(sk::CLOSE_PAREN);
        self.node(sk::N_PARENTHESIZED_EXPRESSION, pos)
    }

    fn parse_spread_element(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::DOT_DOT_DOT);
        self.parse_assignment_expression_or_higher(true);
        self.node(sk::N_SPREAD_ELEMENT, pos)
    }

    fn parse_argument_or_array_literal_element(&mut self) -> Node {
        if self.tok() == sk::DOT_DOT_DOT {
            self.parse_spread_element()
        } else if self.tok() == sk::COMMA {
            let pos = self.full_start;
            self.node(sk::N_OMITTED_EXPRESSION, pos)
        } else {
            self.parse_assignment_expression_or_higher(true)
        }
    }

    fn parse_argument_expression(&mut self) -> Node {
        self.do_outside_of_context(CTX_DISALLOW_IN | CTX_DECORATOR, Self::parse_argument_or_array_literal_element)
    }

    fn parse_array_literal_expression(&mut self) -> Node {
        self.guard(|p| {
            let pos = p.full_start;
            p.parse_expected(sk::OPEN_BRACKET);
            let elements = p.parse_delimited_list(pc::ARRAY_LITERAL_MEMBERS, |p| Some(p.parse_argument_or_array_literal_element()), false);
            p.parse_expected_matching_brackets(sk::CLOSE_BRACKET);
            p.node_with(sk::N_ARRAY_LITERAL_EXPRESSION, pos, Data::Elems(elements.unwrap_or_default()))
        })
    }

    /// `getTextOfPropertyName` for the name of a property assignment, None for a computed name that is
    /// not a string or numeric literal (`isComputedNonLiteralName`).
    fn property_key(name: &Node) -> Option<Vec<u16>> {
        let literal_text = |n: &Node| -> Option<Vec<u16>> {
            match (n.kind, &n.data) {
                (sk::STRING_LITERAL | sk::NO_SUBSTITUTION_TEMPLATE_LITERAL, Data::Text(t)) => Some(t.clone()),
                (sk::NUMERIC_LITERAL, Data::Num(v)) => Some(crate::js::number_to_string(*v).encode_utf16().collect()),
                _ => None,
            }
        };
        match (name.kind, &name.data) {
            (sk::N_COMPUTED_PROPERTY_NAME, Data::Child(expression)) => literal_text(expression),
            (sk::IDENTIFIER | sk::PRIVATE_IDENTIFIER | sk::BIG_INT_LITERAL, Data::Text(t)) => Some(t.clone()),
            _ => literal_text(name),
        }
    }

    /// A member of an object literal that is not a property assignment.
    fn other_member(&self, kind: K) -> Prop {
        Prop { kind, key: None, initializer: self.node(N_OTHER, self.full_start) }
    }

    fn parse_object_literal_element(&mut self) -> Option<Prop> {
        if self.parse_optional_token(sk::DOT_DOT_DOT) {
            self.parse_assignment_expression_or_higher(true);
            return Some(self.other_member(sk::N_SPREAD_ASSIGNMENT));
        }
        let pos = self.full_start;
        let modifiers = self.parse_modifiers(true, false, false);
        if self.parse_contextual_modifier(sk::GET) {
            self.parse_accessor_declaration(pos, sk::N_GET_ACCESSOR, 0);
            return Some(self.other_member(sk::N_GET_ACCESSOR));
        }
        if self.parse_contextual_modifier(sk::SET) {
            self.parse_accessor_declaration(pos, sk::N_SET_ACCESSOR, 0);
            return Some(self.other_member(sk::N_SET_ACCESSOR));
        }
        let asterisk_token = self.parse_optional_token(sk::ASTERISK);
        let token_is_identifier = self.is_identifier();
        let name = self.parse_property_name();
        self.parse_optional_token(sk::QUESTION);
        self.parse_optional_token(sk::EXCLAMATION);
        if asterisk_token || self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN {
            self.parse_method_declaration(pos, asterisk_token, modifiers.has(sk::ASYNC));
            return Some(self.other_member(sk::N_METHOD_DECLARATION));
        }
        if token_is_identifier && self.tok() != sk::COLON {
            if self.parse_optional_token(sk::EQUALS) {
                self.allow_in_and(|p| p.parse_assignment_expression_or_higher(true));
            }
            return Some(self.other_member(sk::N_SHORTHAND_PROPERTY_ASSIGNMENT));
        }
        self.parse_expected(sk::COLON);
        let initializer = self.allow_in_and(|p| p.parse_assignment_expression_or_higher(true));
        Some(Prop { kind: sk::N_PROPERTY_ASSIGNMENT, key: Self::property_key(&name), initializer })
    }

    fn parse_object_literal_expression(&mut self) -> Node {
        self.guard(|p| {
            let pos = p.full_start;
            p.parse_expected(sk::OPEN_BRACE);
            let properties = p.parse_delimited_list(pc::OBJECT_LITERAL_MEMBERS, Self::parse_object_literal_element, true);
            p.parse_expected_matching_brackets(sk::CLOSE_BRACE);
            p.node_with(sk::N_OBJECT_LITERAL_EXPRESSION, pos, Data::Props(properties.unwrap_or_default()))
        })
    }

    fn parse_function_expression(&mut self) -> Node {
        let saved_decorator_context = self.in_decorator_context();
        self.set_context_flag(false, CTX_DECORATOR);
        let pos = self.full_start;
        let modifiers = self.parse_modifiers(false, false, false);
        self.parse_expected(sk::FUNCTION);
        let asterisk_token = self.parse_optional_token(sk::ASTERISK);
        let is_generator = if asterisk_token { SIG_YIELD } else { 0 };
        let is_async = if modifiers.has(sk::ASYNC) { SIG_AWAIT } else { 0 };
        if is_generator != 0 && is_async != 0 {
            self.do_inside_of_context(CTX_YIELD | CTX_AWAIT, Self::parse_optional_binding_identifier);
        } else if is_generator != 0 {
            self.do_inside_of_context(CTX_YIELD, Self::parse_optional_binding_identifier);
        } else if is_async != 0 {
            self.do_inside_of_context(CTX_AWAIT, Self::parse_optional_binding_identifier);
        } else {
            self.parse_optional_binding_identifier();
        }
        self.parse_type_parameters();
        self.parse_parameters(is_generator | is_async);
        self.parse_return_type(sk::COLON, false);
        self.parse_function_block(is_generator | is_async);
        self.set_context_flag(saved_decorator_context, CTX_DECORATOR);
        self.node(sk::N_FUNCTION_EXPRESSION, pos)
    }

    fn parse_optional_binding_identifier(&mut self) -> Option<Node> {
        if self.is_binding_identifier() { Some(self.parse_binding_identifier()) } else { None }
    }

    fn parse_new_expression_or_new_dot_target(&mut self) -> Node {
        self.guard(|p| {
            let pos = p.full_start;
            p.parse_expected(sk::NEW);
            if p.parse_optional(sk::DOT) {
                p.parse_identifier_name();
                return p.node(sk::N_META_PROPERTY, pos);
            }
            let expression_pos = p.full_start;
            let primary = p.parse_primary_expression();
            p.parse_member_expression_rest(expression_pos, primary, false);
            if p.tok() == sk::QUESTION_DOT {
                p.error();
            }
            if p.tok() == sk::OPEN_PAREN {
                p.parse_argument_list();
            }
            p.node(sk::N_NEW_EXPRESSION, pos)
        })
    }
}

// ---------------------------------------------------------------------------------------------------
// The parser: statements, declarations, classes, modules
// ---------------------------------------------------------------------------------------------------

impl Parser<'_> {
    fn next_token_is_open_paren(&mut self) -> bool {
        self.next_token() == sk::OPEN_PAREN
    }

    fn next_token_is_open_brace(&mut self) -> bool {
        self.next_token() == sk::OPEN_BRACE
    }

    fn next_token_is_slash(&mut self) -> bool {
        self.next_token() == sk::SLASH
    }

    fn next_token_is_string_literal(&mut self) -> bool {
        self.next_token() == sk::STRING_LITERAL
    }

    fn next_token_is_identifier_or_keyword_on_same_line(&mut self) -> bool {
        self.next_token();
        token_is_identifier_or_keyword(self.tok()) && !self.has_preceding_line_break()
    }

    fn next_token_is_class_keyword_on_same_line(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::CLASS && !self.has_preceding_line_break()
    }

    fn next_token_is_function_keyword_on_same_line(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::FUNCTION && !self.has_preceding_line_break()
    }

    fn next_token_is_identifier_or_keyword_or_literal_on_same_line(&mut self) -> bool {
        self.next_token();
        (token_is_identifier_or_keyword(self.tok())
            || self.tok() == sk::NUMERIC_LITERAL
            || self.tok() == sk::BIG_INT_LITERAL
            || self.tok() == sk::STRING_LITERAL)
            && !self.has_preceding_line_break()
    }

    fn next_token_is_identifier_on_same_line(&mut self) -> bool {
        self.next_token();
        !self.has_preceding_line_break() && self.is_identifier()
    }

    fn next_token_is_identifier_or_string_literal_on_same_line(&mut self) -> bool {
        self.next_token();
        !self.has_preceding_line_break() && (self.is_identifier() || self.tok() == sk::STRING_LITERAL)
    }

    // ----- blocks and simple statements -----

    fn parse_block(&mut self, ignore_missing_open_brace: bool) -> Node {
        let pos = self.full_start;
        let open_brace_parsed = self.parse_expected(sk::OPEN_BRACE);
        if open_brace_parsed || ignore_missing_open_brace {
            self.parse_list(pc::BLOCK_STATEMENTS, |p| {
                p.parse_statement();
            });
            self.parse_expected_matching_brackets(sk::CLOSE_BRACE);
            let result = self.node(N_OTHER, pos);
            if self.tok() == sk::EQUALS {
                self.error();
                self.next_token();
            }
            result
        } else {
            self.node(N_OTHER, pos)
        }
    }

    fn parse_function_block(&mut self, flags: u32) -> Node {
        let saved_yield_context = self.in_yield_context();
        self.set_context_flag(flags & SIG_YIELD != 0, CTX_YIELD);
        let saved_await_context = self.in_await_context();
        self.set_context_flag(flags & SIG_AWAIT != 0, CTX_AWAIT);
        let save_decorator_context = self.in_decorator_context();
        if save_decorator_context {
            self.set_context_flag(false, CTX_DECORATOR);
        }
        let block = self.parse_block(flags & SIG_IGNORE_MISSING_OPEN_BRACE != 0);
        if save_decorator_context {
            self.set_context_flag(true, CTX_DECORATOR);
        }
        self.set_context_flag(saved_yield_context, CTX_YIELD);
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        block
    }

    fn parse_empty_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::SEMICOLON);
        self.node(N_OTHER, pos)
    }

    fn parse_if_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::IF);
        self.parse_expected(sk::OPEN_PAREN);
        self.allow_in_and(Self::parse_expression);
        self.parse_expected_matching_brackets(sk::CLOSE_PAREN);
        self.parse_statement();
        if self.parse_optional(sk::ELSE) {
            self.parse_statement();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_do_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::DO);
        self.parse_statement();
        self.parse_expected(sk::WHILE);
        self.parse_expected(sk::OPEN_PAREN);
        self.allow_in_and(Self::parse_expression);
        self.parse_expected_matching_brackets(sk::CLOSE_PAREN);
        self.parse_optional(sk::SEMICOLON);
        self.node(N_OTHER, pos)
    }

    fn parse_while_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::WHILE);
        self.parse_expected(sk::OPEN_PAREN);
        self.allow_in_and(Self::parse_expression);
        self.parse_expected_matching_brackets(sk::CLOSE_PAREN);
        self.parse_statement();
        self.node(N_OTHER, pos)
    }

    fn parse_for_or_for_in_or_for_of_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::FOR);
        let await_token = self.parse_optional_token(sk::AWAIT);
        self.parse_expected(sk::OPEN_PAREN);
        if self.tok() != sk::SEMICOLON {
            if self.tok() == sk::VAR
                || self.tok() == sk::LET
                || self.tok() == sk::CONST
                || (self.tok() == sk::USING
                    && self.look_ahead(Self::next_token_is_binding_identifier_or_start_of_destructuring_on_same_line_disallow_of))
                || (self.tok() == sk::AWAIT
                    && self.look_ahead(
                        Self::next_token_is_using_keyword_then_binding_identifier_or_start_of_object_destructuring_on_same_line,
                    ))
            {
                self.parse_variable_declaration_list(true);
            } else {
                self.disallow_in_and(Self::parse_expression);
            }
        }
        let of = if await_token { self.parse_expected(sk::OF) } else { self.parse_optional(sk::OF) };
        if of {
            self.allow_in_and(|p| p.parse_assignment_expression_or_higher(true));
            self.parse_expected(sk::CLOSE_PAREN);
            self.parse_statement();
        } else if self.parse_optional(sk::IN) {
            self.allow_in_and(Self::parse_expression);
            self.parse_expected(sk::CLOSE_PAREN);
            self.parse_statement();
        } else {
            self.parse_expected(sk::SEMICOLON);
            if self.tok() != sk::SEMICOLON && self.tok() != sk::CLOSE_PAREN {
                self.allow_in_and(Self::parse_expression);
            }
            self.parse_expected(sk::SEMICOLON);
            if self.tok() != sk::CLOSE_PAREN {
                self.allow_in_and(Self::parse_expression);
            }
            self.parse_expected(sk::CLOSE_PAREN);
            self.parse_statement();
        }
        self.node(N_OTHER, pos)
    }

    fn next_token_is_binding_identifier_or_start_of_destructuring_on_same_line_disallow_of(&mut self) -> bool {
        self.next_token_is_binding_identifier_or_start_of_destructuring_on_same_line(true)
    }

    fn parse_break_or_continue_statement(&mut self, is_break: bool) -> Node {
        let pos = self.full_start;
        self.parse_expected(if is_break { sk::BREAK } else { sk::CONTINUE });
        if !self.can_parse_semicolon() {
            self.parse_identifier();
        }
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_return_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::RETURN);
        if !self.can_parse_semicolon() {
            self.allow_in_and(Self::parse_expression);
        }
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_with_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::WITH);
        self.parse_expected(sk::OPEN_PAREN);
        self.allow_in_and(Self::parse_expression);
        self.parse_expected_matching_brackets(sk::CLOSE_PAREN);
        self.parse_statement();
        self.node(N_OTHER, pos)
    }

    fn parse_case_or_default_clause(&mut self) {
        if self.tok() == sk::CASE {
            self.parse_expected(sk::CASE);
            self.allow_in_and(Self::parse_expression);
            self.parse_expected(sk::COLON);
        } else {
            self.parse_expected(sk::DEFAULT);
            self.parse_expected(sk::COLON);
        }
        self.parse_list(pc::SWITCH_CLAUSE_STATEMENTS, |p| {
            p.parse_statement();
        });
    }

    fn parse_switch_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::SWITCH);
        self.parse_expected(sk::OPEN_PAREN);
        self.allow_in_and(Self::parse_expression);
        self.parse_expected(sk::CLOSE_PAREN);
        self.parse_expected(sk::OPEN_BRACE);
        self.parse_list(pc::SWITCH_CLAUSES, Self::parse_case_or_default_clause);
        self.parse_expected(sk::CLOSE_BRACE);
        self.node(N_OTHER, pos)
    }

    fn parse_throw_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::THROW);
        let expression = if self.has_preceding_line_break() {
            let at = self.full_start;
            self.create_missing_node_silent(at)
        } else {
            self.allow_in_and(Self::parse_expression)
        };
        if !self.try_parse_semicolon() {
            self.parse_error_for_missing_semicolon_after(&expression);
        }
        self.node(N_OTHER, pos)
    }

    /// `createIdentifier("")` without a diagnostic.
    fn create_missing_node_silent(&self, pos: usize) -> Node {
        Node { kind: sk::IDENTIFIER, pos, end: pos, op: 0, data: Data::Text(Vec::new()) }
    }

    fn parse_try_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::TRY);
        self.parse_block(false);
        let catch_clause = self.tok() == sk::CATCH;
        if catch_clause {
            self.parse_catch_clause();
        }
        if !catch_clause || self.tok() == sk::FINALLY {
            self.parse_expected(sk::FINALLY);
            self.parse_block(false);
        }
        self.node(N_OTHER, pos)
    }

    fn parse_catch_clause(&mut self) {
        self.parse_expected(sk::CATCH);
        if self.parse_optional(sk::OPEN_PAREN) {
            self.parse_variable_declaration(false);
            self.parse_expected(sk::CLOSE_PAREN);
        }
        self.parse_block(false);
    }

    fn parse_debugger_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::DEBUGGER);
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_expression_or_labeled_statement(&mut self) -> Node {
        let pos = self.full_start;
        let expression = self.allow_in_and(Self::parse_expression);
        if expression.kind == sk::IDENTIFIER && self.parse_optional(sk::COLON) {
            self.parse_statement();
        } else if !self.try_parse_semicolon() {
            self.parse_error_for_missing_semicolon_after(&expression);
        }
        self.node(N_OTHER, pos)
    }

    // ----- statement starts -----

    fn is_declaration(&mut self) -> bool {
        loop {
            match self.tok() {
                sk::VAR | sk::LET | sk::CONST | sk::FUNCTION | sk::CLASS | sk::ENUM => return true,
                sk::USING => return self.is_using_declaration(),
                sk::AWAIT => return self.is_await_using_declaration(),
                sk::INTERFACE | sk::TYPE | sk::DEFER => {
                    return self.next_token_is_identifier_on_same_line();
                }
                sk::MODULE | sk::NAMESPACE => {
                    return self.next_token_is_identifier_or_string_literal_on_same_line();
                }
                sk::ABSTRACT | sk::ACCESSOR | sk::ASYNC | sk::DECLARE | sk::PRIVATE | sk::PROTECTED | sk::PUBLIC | sk::READONLY => {
                    let previous_token = self.tok();
                    self.next_token();
                    if self.has_preceding_line_break() {
                        return false;
                    }
                    if previous_token == sk::DECLARE && self.tok() == sk::TYPE {
                        return true;
                    }
                }
                sk::GLOBAL => {
                    self.next_token();
                    return self.tok() == sk::OPEN_BRACE || self.tok() == sk::IDENTIFIER || self.tok() == sk::EXPORT;
                }
                sk::IMPORT => {
                    self.next_token();
                    return self.tok() == sk::DEFER
                        || self.tok() == sk::STRING_LITERAL
                        || self.tok() == sk::ASTERISK
                        || self.tok() == sk::OPEN_BRACE
                        || token_is_identifier_or_keyword(self.tok());
                }
                sk::EXPORT => {
                    let mut current_token = self.next_token();
                    if current_token == sk::TYPE {
                        current_token = self.look_ahead(Self::next_token);
                    }
                    if current_token == sk::EQUALS
                        || current_token == sk::ASTERISK
                        || current_token == sk::OPEN_BRACE
                        || current_token == sk::DEFAULT
                        || current_token == sk::AS
                        || current_token == sk::AT
                    {
                        return true;
                    }
                }
                sk::STATIC => {
                    self.next_token();
                }
                _ => return false,
            }
        }
    }

    fn is_start_of_declaration(&mut self) -> bool {
        self.look_ahead(Self::is_declaration)
    }

    fn is_start_of_statement(&mut self) -> bool {
        match self.tok() {
            sk::AT
            | sk::SEMICOLON
            | sk::OPEN_BRACE
            | sk::VAR
            | sk::LET
            | sk::USING
            | sk::FUNCTION
            | sk::CLASS
            | sk::ENUM
            | sk::IF
            | sk::DO
            | sk::WHILE
            | sk::FOR
            | sk::CONTINUE
            | sk::BREAK
            | sk::RETURN
            | sk::WITH
            | sk::SWITCH
            | sk::THROW
            | sk::TRY
            | sk::DEBUGGER
            | sk::CATCH
            | sk::FINALLY => true,
            sk::IMPORT => self.is_start_of_declaration() || self.look_ahead(Self::next_token_is_open_paren_or_less_than_or_dot),
            sk::CONST | sk::EXPORT => self.is_start_of_declaration(),
            sk::ASYNC | sk::DECLARE | sk::INTERFACE | sk::MODULE | sk::NAMESPACE | sk::TYPE | sk::GLOBAL | sk::DEFER => true,
            sk::ACCESSOR | sk::PUBLIC | sk::PRIVATE | sk::PROTECTED | sk::STATIC | sk::READONLY => {
                self.is_start_of_declaration() || !self.look_ahead(Self::next_token_is_identifier_or_keyword_on_same_line)
            }
            _ => self.is_start_of_expression(),
        }
    }

    fn next_token_is_binding_identifier_or_start_of_destructuring(&mut self) -> bool {
        self.next_token();
        self.is_binding_identifier() || self.tok() == sk::OPEN_BRACE || self.tok() == sk::OPEN_BRACKET
    }

    fn is_let_declaration(&mut self) -> bool {
        self.look_ahead(Self::next_token_is_binding_identifier_or_start_of_destructuring)
    }

    fn next_token_is_equals_or_semicolon_or_colon_token(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::EQUALS || self.tok() == sk::SEMICOLON || self.tok() == sk::COLON
    }

    fn next_token_is_binding_identifier_or_start_of_destructuring_on_same_line(&mut self, disallow_of: bool) -> bool {
        self.next_token();
        if disallow_of && self.tok() == sk::OF {
            return self.look_ahead(Self::next_token_is_equals_or_semicolon_or_colon_token);
        }
        (self.is_binding_identifier() || self.tok() == sk::OPEN_BRACE) && !self.has_preceding_line_break()
    }

    fn next_token_is_binding_identifier_or_start_of_destructuring_on_same_line_allow_of(&mut self) -> bool {
        self.next_token_is_binding_identifier_or_start_of_destructuring_on_same_line(false)
    }

    fn is_using_declaration(&mut self) -> bool {
        self.look_ahead(Self::next_token_is_binding_identifier_or_start_of_destructuring_on_same_line_allow_of)
    }

    fn next_token_is_using_keyword_then_binding_identifier_or_start_of_object_destructuring_on_same_line(&mut self) -> bool {
        if self.next_token() == sk::USING {
            return self.next_token_is_binding_identifier_or_start_of_destructuring_on_same_line(false);
        }
        false
    }

    fn is_await_using_declaration(&mut self) -> bool {
        self.look_ahead(Self::next_token_is_using_keyword_then_binding_identifier_or_start_of_object_destructuring_on_same_line)
    }

    /// `parseStatement`. A statement the parser can't start (`undefined`) consumes nothing.
    fn parse_statement(&mut self) -> Option<Node> {
        self.guard(|p| p.parse_statement_worker())
    }

    fn parse_statement_worker(&mut self) -> Option<Node> {
        match self.tok() {
            sk::SEMICOLON => return Some(self.parse_empty_statement()),
            sk::OPEN_BRACE => return Some(self.parse_block(false)),
            sk::VAR => return Some(self.parse_variable_statement()),
            sk::LET => {
                if self.is_let_declaration() {
                    return Some(self.parse_variable_statement());
                }
            }
            sk::AWAIT => {
                if self.is_await_using_declaration() {
                    return Some(self.parse_variable_statement());
                }
            }
            sk::USING => {
                if self.is_using_declaration() {
                    return Some(self.parse_variable_statement());
                }
            }
            sk::FUNCTION => return Some(self.parse_function_declaration(&Modifiers::default())),
            sk::CLASS => {
                return Some(self.parse_class_declaration_or_expression(&Modifiers::default(), N_OTHER));
            }
            sk::IF => return Some(self.parse_if_statement()),
            sk::DO => return Some(self.parse_do_statement()),
            sk::WHILE => return Some(self.parse_while_statement()),
            sk::FOR => return Some(self.parse_for_or_for_in_or_for_of_statement()),
            sk::CONTINUE => return Some(self.parse_break_or_continue_statement(false)),
            sk::BREAK => return Some(self.parse_break_or_continue_statement(true)),
            sk::RETURN => return Some(self.parse_return_statement()),
            sk::WITH => return Some(self.parse_with_statement()),
            sk::SWITCH => return Some(self.parse_switch_statement()),
            sk::THROW => return Some(self.parse_throw_statement()),
            sk::TRY | sk::CATCH | sk::FINALLY => return Some(self.parse_try_statement()),
            sk::DEBUGGER => return Some(self.parse_debugger_statement()),
            sk::AT => return self.parse_declaration(),
            sk::ASYNC
            | sk::INTERFACE
            | sk::TYPE
            | sk::MODULE
            | sk::NAMESPACE
            | sk::DECLARE
            | sk::CONST
            | sk::ENUM
            | sk::EXPORT
            | sk::IMPORT
            | sk::PRIVATE
            | sk::PROTECTED
            | sk::PUBLIC
            | sk::ABSTRACT
            | sk::ACCESSOR
            | sk::STATIC
            | sk::READONLY
            | sk::GLOBAL
                if self.is_start_of_declaration() =>
            {
                return self.parse_declaration();
            }
            _ => {}
        }
        Some(self.parse_expression_or_labeled_statement())
    }

    fn parse_declaration(&mut self) -> Option<Node> {
        let pos = self.full_start;
        let modifiers = self.parse_modifiers(true, false, false);
        self.parse_declaration_worker(pos, &modifiers)
    }

    fn parse_declaration_worker(&mut self, pos: usize, modifiers_in: &Modifiers) -> Option<Node> {
        match self.tok() {
            sk::VAR | sk::LET | sk::CONST | sk::USING | sk::AWAIT => Some(self.parse_variable_statement()),
            sk::FUNCTION => Some(self.parse_function_declaration(modifiers_in)),
            sk::CLASS => Some(self.parse_class_declaration_or_expression(modifiers_in, N_OTHER)),
            sk::INTERFACE => Some(self.parse_interface_declaration()),
            sk::TYPE => Some(self.parse_type_alias_declaration()),
            sk::ENUM => Some(self.parse_enum_declaration()),
            sk::GLOBAL | sk::MODULE | sk::NAMESPACE => Some(self.parse_module_declaration()),
            sk::IMPORT => Some(self.parse_import_declaration_or_import_equals_declaration()),
            sk::EXPORT => {
                self.next_token();
                match self.tok() {
                    sk::DEFAULT | sk::EQUALS => Some(self.parse_export_assignment()),
                    sk::AS => Some(self.parse_namespace_export_declaration()),
                    _ => Some(self.parse_export_declaration()),
                }
            }
            _ => {
                if modifiers_in.some() {
                    self.create_missing_node(sk::N_MISSING_DECLARATION, true, true);
                    return Some(self.node(sk::N_MISSING_DECLARATION, pos));
                }
                None
            }
        }
    }

    fn parse_function_block_or_semicolon(&mut self, flags: u32) {
        if self.tok() != sk::OPEN_BRACE {
            if flags & SIG_TYPE != 0 {
                self.parse_type_member_semicolon();
                return;
            }
            if self.can_parse_semicolon() {
                self.parse_semicolon();
                return;
            }
        }
        self.parse_function_block(flags);
    }

    // ----- binding patterns and variables -----

    fn parse_array_binding_element(&mut self) -> Option<Node> {
        let pos = self.full_start;
        if self.tok() == sk::COMMA {
            return Some(self.node(sk::N_OMITTED_EXPRESSION, pos));
        }
        self.parse_optional_token(sk::DOT_DOT_DOT);
        self.parse_identifier_or_pattern();
        self.parse_initializer();
        Some(self.node(N_OTHER, pos))
    }

    fn parse_object_binding_element(&mut self) -> Option<Node> {
        let pos = self.full_start;
        self.parse_optional_token(sk::DOT_DOT_DOT);
        let token_is_identifier = self.is_binding_identifier();
        self.parse_property_name();
        if !(token_is_identifier && self.tok() != sk::COLON) {
            self.parse_expected(sk::COLON);
            self.parse_identifier_or_pattern();
        }
        self.parse_initializer();
        Some(self.node(N_OTHER, pos))
    }

    fn parse_object_binding_pattern(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::OPEN_BRACE);
        self.allow_in_and(|p| p.parse_delimited_list(pc::OBJECT_BINDING_ELEMENTS, Self::parse_object_binding_element, false));
        self.parse_expected(sk::CLOSE_BRACE);
        self.node(N_OTHER, pos)
    }

    fn parse_array_binding_pattern(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::OPEN_BRACKET);
        self.allow_in_and(|p| p.parse_delimited_list(pc::ARRAY_BINDING_ELEMENTS, Self::parse_array_binding_element, false));
        self.parse_expected(sk::CLOSE_BRACKET);
        self.node(N_OTHER, pos)
    }

    fn is_binding_identifier_or_private_identifier_or_pattern(&self) -> bool {
        self.tok() == sk::OPEN_BRACE
            || self.tok() == sk::OPEN_BRACKET
            || self.tok() == sk::PRIVATE_IDENTIFIER
            || self.is_binding_identifier()
    }

    fn parse_identifier_or_pattern(&mut self) -> Node {
        self.guard(|p| {
            if p.tok() == sk::OPEN_BRACKET {
                return p.parse_array_binding_pattern();
            }
            if p.tok() == sk::OPEN_BRACE {
                return p.parse_object_binding_pattern();
            }
            p.parse_binding_identifier()
        })
    }

    fn parse_variable_declaration_allow_exclamation(&mut self) -> Option<Node> {
        Some(self.parse_variable_declaration(true))
    }

    fn parse_variable_declaration_no_exclamation(&mut self) -> Option<Node> {
        Some(self.parse_variable_declaration(false))
    }

    fn parse_variable_declaration(&mut self, allow_exclamation: bool) -> Node {
        let pos = self.full_start;
        let name = self.parse_identifier_or_pattern();
        if allow_exclamation && name.kind == sk::IDENTIFIER && self.tok() == sk::EXCLAMATION && !self.has_preceding_line_break() {
            self.parse_token_node();
        }
        self.parse_type_annotation();
        if !(self.tok() == sk::IN || self.tok() == sk::OF) {
            self.parse_initializer();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_variable_declaration_list(&mut self, in_for_statement_initializer: bool) -> Node {
        let pos = self.full_start;
        match self.tok() {
            sk::VAR | sk::LET | sk::CONST | sk::USING => {}
            sk::AWAIT => {
                if !self.is_await_using_declaration() {
                    self.throw("Debug Failure. False expression.");
                }
                self.next_token();
            }
            _ => self.throw("Debug Failure."),
        }
        self.next_token();
        if self.tok() == sk::OF && self.look_ahead(Self::can_follow_contextual_of_keyword) {
            // a missing declaration list
        } else {
            let saved_disallow_in = self.in_disallow_in_context();
            self.set_context_flag(in_for_statement_initializer, CTX_DISALLOW_IN);
            if in_for_statement_initializer {
                self.parse_delimited_list(pc::VARIABLE_DECLARATIONS, Self::parse_variable_declaration_no_exclamation, false);
            } else {
                self.parse_delimited_list(pc::VARIABLE_DECLARATIONS, Self::parse_variable_declaration_allow_exclamation, false);
            }
            self.set_context_flag(saved_disallow_in, CTX_DISALLOW_IN);
        }
        self.node(N_OTHER, pos)
    }

    fn can_follow_contextual_of_keyword(&mut self) -> bool {
        self.next_token_is_identifier() && self.next_token() == sk::CLOSE_PAREN
    }

    fn parse_variable_statement(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_variable_declaration_list(false);
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_function_declaration(&mut self, modifiers: &Modifiers) -> Node {
        let pos = self.full_start;
        let saved_await_context = self.in_await_context();
        self.parse_expected(sk::FUNCTION);
        let asterisk_token = self.parse_optional_token(sk::ASTERISK);
        if modifiers.has(sk::DEFAULT) {
            self.parse_optional_binding_identifier();
        } else {
            self.parse_binding_identifier();
        }
        let is_generator = if asterisk_token { SIG_YIELD } else { 0 };
        let is_async = if modifiers.has(sk::ASYNC) { SIG_AWAIT } else { 0 };
        self.parse_type_parameters();
        if modifiers.has(sk::EXPORT) {
            self.set_context_flag(true, CTX_AWAIT);
        }
        self.parse_parameters(is_generator | is_async);
        self.parse_return_type(sk::COLON, false);
        self.parse_function_block_or_semicolon(is_generator | is_async);
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        self.node(N_OTHER, pos)
    }

    fn parse_constructor_name(&mut self) -> bool {
        if self.tok() == sk::CONSTRUCTOR {
            return self.parse_expected(sk::CONSTRUCTOR);
        }
        if self.tok() == sk::STRING_LITERAL && self.look_ahead(Self::next_token) == sk::OPEN_PAREN {
            return self.try_parse_bool(|p| {
                let literal_node = p.parse_literal_node();
                matches!(&literal_node.data, Data::Text(t) if t.iter().copied().eq("constructor".encode_utf16()))
            });
        }
        false
    }

    fn try_parse_constructor_declaration(&mut self, pos: usize) -> Option<Node> {
        self.try_parse(|p| {
            if p.parse_constructor_name() {
                p.parse_type_parameters();
                p.parse_parameters(0);
                p.parse_return_type(sk::COLON, false);
                p.parse_function_block_or_semicolon(0);
                return Some(p.node(N_OTHER, pos));
            }
            None
        })
    }

    fn parse_method_declaration(&mut self, pos: usize, asterisk_token: bool, is_async_modifier: bool) -> Node {
        let is_generator = if asterisk_token { SIG_YIELD } else { 0 };
        let is_async = if is_async_modifier { SIG_AWAIT } else { 0 };
        self.parse_type_parameters();
        self.parse_parameters(is_generator | is_async);
        self.parse_return_type(sk::COLON, false);
        self.parse_function_block_or_semicolon(is_generator | is_async);
        self.node(N_OTHER, pos)
    }

    fn parse_property_declaration(&mut self, pos: usize, name: &Node, question_token: bool) -> Node {
        if !question_token && !self.has_preceding_line_break() {
            self.parse_optional_token(sk::EXCLAMATION);
        }
        let ty = self.parse_type_annotation();
        let initializer = self.do_outside_of_context(CTX_YIELD | CTX_AWAIT | CTX_DISALLOW_IN, Self::parse_initializer);
        self.parse_semicolon_after_property_name(name, ty.is_some(), initializer.is_some());
        self.node(N_OTHER, pos)
    }

    fn parse_property_or_method_declaration(&mut self, pos: usize, modifiers: &Modifiers) -> Node {
        let asterisk_token = self.parse_optional_token(sk::ASTERISK);
        let name = self.parse_property_name();
        let question_token = self.parse_optional_token(sk::QUESTION);
        if asterisk_token || self.tok() == sk::OPEN_PAREN || self.tok() == sk::LESS_THAN {
            return self.parse_method_declaration(pos, asterisk_token, modifiers.has(sk::ASYNC));
        }
        self.parse_property_declaration(pos, &name, question_token)
    }

    fn parse_accessor_declaration(&mut self, pos: usize, _kind: K, flags: u32) -> Node {
        self.parse_property_name();
        self.parse_type_parameters();
        self.parse_parameters(0);
        self.parse_return_type(sk::COLON, false);
        self.parse_function_block_or_semicolon(flags);
        self.node(N_OTHER, pos)
    }

    fn is_class_member_start(&mut self) -> bool {
        let mut id_token: Option<K> = None;
        if self.tok() == sk::AT {
            return true;
        }
        while is_modifier_kind(self.tok()) {
            id_token = Some(self.tok());
            if is_class_member_modifier(self.tok()) {
                return true;
            }
            self.next_token();
        }
        if self.tok() == sk::ASTERISK {
            return true;
        }
        if self.is_literal_property_name() {
            id_token = Some(self.tok());
            self.next_token();
        }
        if self.tok() == sk::OPEN_BRACKET {
            return true;
        }
        if let Some(id_token) = id_token {
            if !is_keyword(id_token) || id_token == sk::SET || id_token == sk::GET {
                return true;
            }
            return match self.tok() {
                sk::OPEN_PAREN | sk::LESS_THAN | sk::EXCLAMATION | sk::COLON | sk::EQUALS | sk::QUESTION => true,
                _ => self.can_parse_semicolon(),
            };
        }
        false
    }

    fn parse_class_static_block_declaration(&mut self, pos: usize) -> Node {
        self.parse_expected_token(sk::STATIC);
        let saved_yield_context = self.in_yield_context();
        let saved_await_context = self.in_await_context();
        self.set_context_flag(false, CTX_YIELD);
        self.set_context_flag(true, CTX_AWAIT);
        self.parse_block(false);
        self.set_context_flag(saved_yield_context, CTX_YIELD);
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        self.node(N_OTHER, pos)
    }

    fn parse_decorator_expression(&mut self) -> Node {
        if self.in_await_context() && self.tok() == sk::AWAIT {
            let pos = self.full_start;
            let await_expression = self.parse_identifier();
            self.next_token();
            let member_expression = self.parse_member_expression_rest(pos, await_expression, true);
            return self.parse_call_expression_rest(pos, member_expression);
        }
        self.parse_left_hand_side_expression_or_higher()
    }

    fn try_parse_decorator(&mut self) -> Option<Node> {
        let pos = self.full_start;
        if !self.parse_optional(sk::AT) {
            return None;
        }
        self.do_inside_of_context(CTX_DECORATOR, Self::parse_decorator_expression);
        Some(self.node(sk::N_DECORATOR, pos))
    }

    fn try_parse_modifier(
        &mut self,
        has_seen_static_modifier: bool,
        permit_const_as_modifier: bool,
        stop_on_start_of_class_static_block: bool,
    ) -> Option<K> {
        let kind = self.tok();
        let not_a_modifier = if self.tok() == sk::CONST && permit_const_as_modifier {
            !self.try_parse_bool(Self::next_token_is_on_same_line_and_can_follow_modifier)
        } else if (stop_on_start_of_class_static_block && self.tok() == sk::STATIC && self.look_ahead(Self::next_token_is_open_brace))
            || (has_seen_static_modifier && self.tok() == sk::STATIC)
        {
            true
        } else {
            !self.parse_any_contextual_modifier()
        };
        if not_a_modifier { None } else { Some(kind) }
    }

    fn parse_modifiers(
        &mut self,
        allow_decorators: bool,
        permit_const_as_modifier: bool,
        stop_on_start_of_class_static_block: bool,
    ) -> Modifiers {
        let mut list = Modifiers::default();
        let mut has_seen_static_modifier = false;
        let mut has_leading_modifier = false;
        let mut has_trailing_decorator = false;
        if allow_decorators && self.tok() == sk::AT {
            while self.try_parse_decorator().is_some() {
                list.kinds.push(sk::N_DECORATOR);
            }
        }
        while let Some(modifier) =
            self.try_parse_modifier(has_seen_static_modifier, permit_const_as_modifier, stop_on_start_of_class_static_block)
        {
            if modifier == sk::STATIC {
                has_seen_static_modifier = true;
            }
            list.kinds.push(modifier);
            has_leading_modifier = true;
        }
        if has_leading_modifier && allow_decorators && self.tok() == sk::AT {
            while self.try_parse_decorator().is_some() {
                list.kinds.push(sk::N_DECORATOR);
                has_trailing_decorator = true;
            }
        }
        if has_trailing_decorator {
            while let Some(modifier) =
                self.try_parse_modifier(has_seen_static_modifier, permit_const_as_modifier, stop_on_start_of_class_static_block)
            {
                if modifier == sk::STATIC {
                    has_seen_static_modifier = true;
                }
                list.kinds.push(modifier);
            }
        }
        list
    }

    fn parse_modifiers_for_arrow_function(&mut self) -> Modifiers {
        let mut modifiers = Modifiers::default();
        if self.tok() == sk::ASYNC {
            self.next_token();
            modifiers.kinds.push(sk::ASYNC);
        }
        modifiers
    }

    fn parse_class_element(&mut self) -> Option<Node> {
        let pos = self.full_start;
        if self.tok() == sk::SEMICOLON {
            self.next_token();
            return Some(self.node(N_OTHER, pos));
        }
        let modifiers = self.parse_modifiers(true, true, true);
        if self.tok() == sk::STATIC && self.look_ahead(Self::next_token_is_open_brace) {
            return Some(self.parse_class_static_block_declaration(pos));
        }
        if self.parse_contextual_modifier(sk::GET) {
            return Some(self.parse_accessor_declaration(pos, sk::N_GET_ACCESSOR, 0));
        }
        if self.parse_contextual_modifier(sk::SET) {
            return Some(self.parse_accessor_declaration(pos, sk::N_SET_ACCESSOR, 0));
        }
        if (self.tok() == sk::CONSTRUCTOR || self.tok() == sk::STRING_LITERAL)
            && let Some(constructor_declaration) = self.try_parse_constructor_declaration(pos)
        {
            return Some(constructor_declaration);
        }
        if self.is_index_signature() {
            return self.parse_index_signature_declaration(pos);
        }
        if token_is_identifier_or_keyword(self.tok())
            || self.tok() == sk::STRING_LITERAL
            || self.tok() == sk::NUMERIC_LITERAL
            || self.tok() == sk::BIG_INT_LITERAL
            || self.tok() == sk::ASTERISK
            || self.tok() == sk::OPEN_BRACKET
        {
            return Some(self.parse_property_or_method_declaration(pos, &modifiers));
        }
        if modifiers.some() {
            let name = self.create_missing_node(sk::IDENTIFIER, true, true);
            return Some(self.parse_property_declaration(pos, &name, false));
        }
        self.throw("Debug Failure. Should not have attempted to parse class member declaration.");
        None
    }

    fn parse_decorated_expression(&mut self) -> Node {
        let pos = self.full_start;
        let modifiers = self.parse_modifiers(true, false, false);
        if self.tok() == sk::CLASS {
            return self.parse_class_declaration_or_expression(&modifiers, sk::N_CLASS_EXPRESSION);
        }
        self.create_missing_node(sk::N_MISSING_DECLARATION, true, true);
        self.node(sk::N_MISSING_DECLARATION, pos)
    }

    /// A class declaration or expression; `kind` is the kind the node gets.
    fn parse_class_declaration_or_expression(&mut self, modifiers: &Modifiers, kind: K) -> Node {
        let pos = self.full_start;
        let saved_await_context = self.in_await_context();
        self.parse_expected(sk::CLASS);
        self.parse_name_of_class_declaration_or_expression();
        self.parse_type_parameters();
        if modifiers.has(sk::EXPORT) {
            self.set_context_flag(true, CTX_AWAIT);
        }
        self.parse_heritage_clauses();
        if self.parse_expected(sk::OPEN_BRACE) {
            self.parse_list(pc::CLASS_MEMBERS, |p| {
                p.parse_class_element();
            });
            self.parse_expected(sk::CLOSE_BRACE);
        }
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        self.node(kind, pos)
    }

    fn parse_name_of_class_declaration_or_expression(&mut self) {
        if self.is_binding_identifier() && !self.is_implements_clause() {
            let is = self.is_binding_identifier();
            self.create_identifier(is);
        }
    }

    fn is_implements_clause(&mut self) -> bool {
        self.tok() == sk::IMPLEMENTS && self.look_ahead(Self::next_token_is_identifier_or_keyword)
    }

    fn parse_heritage_clauses(&mut self) {
        if self.is_heritage_clause() {
            self.parse_list(pc::HERITAGE_CLAUSES, |p| {
                p.parse_heritage_clause();
            });
        }
    }

    fn parse_heritage_clause(&mut self) -> Node {
        let pos = self.full_start;
        self.next_token();
        self.parse_delimited_list(pc::HERITAGE_CLAUSE_ELEMENT, |p| Some(p.parse_expression_with_type_arguments()), false);
        self.node(N_OTHER, pos)
    }

    fn parse_expression_with_type_arguments(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_left_hand_side_expression_or_higher();
        self.try_parse_type_arguments();
        self.node(N_OTHER, pos)
    }

    fn is_heritage_clause(&self) -> bool {
        self.tok() == sk::EXTENDS || self.tok() == sk::IMPLEMENTS
    }

    fn parse_interface_declaration(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::INTERFACE);
        self.parse_identifier();
        self.parse_type_parameters();
        self.parse_heritage_clauses();
        self.parse_object_type_members();
        self.node(N_OTHER, pos)
    }

    fn parse_type_alias_declaration(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::TYPE);
        if self.has_preceding_line_break() {
            self.error();
        }
        self.parse_identifier();
        self.parse_type_parameters();
        self.parse_expected(sk::EQUALS);
        if !(self.tok() == sk::INTRINSIC && self.try_parse(Self::parse_keyword_and_no_dot).is_some()) {
            self.parse_type();
        }
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_enum_member(&mut self) -> Option<Node> {
        let pos = self.full_start;
        self.parse_property_name();
        self.allow_in_and(Self::parse_initializer);
        Some(self.node(N_OTHER, pos))
    }

    fn parse_enum_declaration(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::ENUM);
        self.parse_identifier();
        if self.parse_expected(sk::OPEN_BRACE) {
            self.do_outside_of_context(CTX_YIELD | CTX_AWAIT, |p| p.parse_delimited_list(pc::ENUM_MEMBERS, Self::parse_enum_member, false));
            self.parse_expected(sk::CLOSE_BRACE);
        }
        self.node(N_OTHER, pos)
    }

    fn parse_module_block(&mut self) -> Node {
        let pos = self.full_start;
        if self.parse_expected(sk::OPEN_BRACE) {
            self.parse_list(pc::BLOCK_STATEMENTS, |p| {
                p.parse_statement();
            });
            self.parse_expected(sk::CLOSE_BRACE);
        }
        self.node(N_OTHER, pos)
    }

    fn parse_module_or_namespace_declaration(&mut self, nested_namespace: bool) -> Node {
        self.guard(|p| {
            let pos = p.full_start;
            if nested_namespace {
                p.parse_identifier_name();
            } else {
                p.parse_identifier();
            }
            if p.parse_optional(sk::DOT) {
                p.parse_module_or_namespace_declaration(true);
            } else {
                p.parse_module_block();
            }
            p.node(N_OTHER, pos)
        })
    }

    fn parse_ambient_external_module_declaration(&mut self) -> Node {
        let pos = self.full_start;
        if self.tok() == sk::GLOBAL {
            self.parse_identifier();
        } else {
            self.parse_literal_node();
        }
        if self.tok() == sk::OPEN_BRACE {
            self.parse_module_block();
        } else {
            self.parse_semicolon();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_module_declaration(&mut self) -> Node {
        if self.tok() == sk::GLOBAL {
            return self.parse_ambient_external_module_declaration();
        } else if self.parse_optional(sk::NAMESPACE) {
            // a namespace
        } else {
            self.parse_expected(sk::MODULE);
            if self.tok() == sk::STRING_LITERAL {
                return self.parse_ambient_external_module_declaration();
            }
        }
        self.parse_module_or_namespace_declaration(false)
    }

    fn is_external_module_reference(&mut self) -> bool {
        self.tok() == sk::REQUIRE && self.look_ahead(Self::next_token_is_open_paren)
    }

    fn parse_namespace_export_declaration(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::AS);
        self.parse_expected(sk::NAMESPACE);
        self.parse_identifier();
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_import_declaration_or_import_equals_declaration(&mut self) -> Node {
        let pos = self.full_start;
        self.parse_expected(sk::IMPORT);
        let mut identifier: Option<Node> = None;
        if self.is_identifier() {
            identifier = Some(self.parse_identifier());
        }
        let identifier_text_is = |id: &Option<Node>, text: &str| -> bool {
            matches!(id, Some(Node { data: Data::Text(t), .. }) if t.iter().copied().eq(text.encode_utf16()))
        };
        let mut is_defer = false;
        if identifier_text_is(&identifier, "type")
            && (self.tok() != sk::FROM || (self.is_identifier() && self.look_ahead(Self::next_token_is_from_keyword_or_equals_token)))
            && (self.is_identifier() || self.token_after_import_definitely_produces_import_declaration())
        {
            identifier = if self.is_identifier() { Some(self.parse_identifier()) } else { None };
        } else if identifier_text_is(&identifier, "defer")
            && (if self.tok() == sk::FROM {
                !self.look_ahead(Self::next_token_is_string_literal)
            } else {
                self.tok() != sk::COMMA && self.tok() != sk::EQUALS
            })
        {
            is_defer = true;
            identifier = if self.is_identifier() { Some(self.parse_identifier()) } else { None };
        }
        if identifier.is_some() && !self.token_after_imported_identifier_definitely_produces_import_declaration() && !is_defer {
            return self.parse_import_equals_declaration(pos);
        }
        self.try_parse_import_clause(identifier.is_some());
        self.parse_module_specifier();
        self.try_parse_import_attributes();
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn next_token_is_from_keyword_or_equals_token(&mut self) -> bool {
        self.next_token();
        self.tok() == sk::FROM || self.tok() == sk::EQUALS
    }

    fn try_parse_import_clause(&mut self, has_identifier: bool) {
        if has_identifier || self.tok() == sk::ASTERISK || self.tok() == sk::OPEN_BRACE {
            self.parse_import_clause(has_identifier);
            self.parse_expected(sk::FROM);
        }
    }

    fn try_parse_import_attributes(&mut self) {
        let current_token = self.tok();
        if (current_token == sk::WITH || current_token == sk::ASSERT) && !self.has_preceding_line_break() {
            self.parse_import_attributes(current_token, false);
        }
    }

    fn parse_import_attribute(&mut self) -> Option<Node> {
        let pos = self.full_start;
        if token_is_identifier_or_keyword(self.tok()) {
            self.parse_identifier_name();
        } else {
            self.parse_literal_like_node(sk::STRING_LITERAL);
        }
        self.parse_expected(sk::COLON);
        self.parse_assignment_expression_or_higher(true);
        Some(self.node(N_OTHER, pos))
    }

    fn parse_import_attributes(&mut self, token: K, skip_keyword: bool) -> Node {
        let pos = self.full_start;
        if !skip_keyword {
            self.parse_expected(token);
        }
        if self.parse_expected(sk::OPEN_BRACE) {
            self.parse_delimited_list(pc::IMPORT_ATTRIBUTES, Self::parse_import_attribute, true);
            self.parse_expected(sk::CLOSE_BRACE);
        }
        self.node(N_OTHER, pos)
    }

    fn token_after_import_definitely_produces_import_declaration(&self) -> bool {
        self.tok() == sk::ASTERISK || self.tok() == sk::OPEN_BRACE
    }

    fn token_after_imported_identifier_definitely_produces_import_declaration(&self) -> bool {
        self.tok() == sk::COMMA || self.tok() == sk::FROM
    }

    fn parse_import_equals_declaration(&mut self, pos: usize) -> Node {
        self.parse_expected(sk::EQUALS);
        self.parse_module_reference();
        self.parse_semicolon();
        self.node(N_OTHER, pos)
    }

    fn parse_import_clause(&mut self, has_identifier: bool) {
        if !has_identifier || self.parse_optional(sk::COMMA) {
            if self.tok() == sk::ASTERISK {
                self.parse_namespace_import();
            } else {
                self.parse_named_imports_or_exports(true);
            }
        }
    }

    fn parse_module_reference(&mut self) {
        if self.is_external_module_reference() {
            self.parse_external_module_reference();
        } else {
            self.parse_entity_name(false);
        }
    }

    fn parse_external_module_reference(&mut self) {
        self.parse_expected(sk::REQUIRE);
        self.parse_expected(sk::OPEN_PAREN);
        self.parse_module_specifier();
        self.parse_expected(sk::CLOSE_PAREN);
    }

    fn parse_module_specifier(&mut self) {
        if self.tok() == sk::STRING_LITERAL {
            self.parse_literal_node();
        } else {
            self.parse_expression();
        }
    }

    fn parse_namespace_import(&mut self) {
        self.parse_expected(sk::ASTERISK);
        self.parse_expected(sk::AS);
        self.parse_identifier();
    }

    fn can_parse_module_export_name(&self) -> bool {
        token_is_identifier_or_keyword(self.tok()) || self.tok() == sk::STRING_LITERAL
    }

    fn parse_module_export_name(&mut self, parse_name: fn(&mut Self) -> Node) -> Node {
        if self.tok() == sk::STRING_LITERAL { self.parse_literal_node() } else { parse_name(self) }
    }

    fn parse_named_imports_or_exports(&mut self, is_import: bool) {
        if is_import {
            self.parse_bracketed_list(
                pc::IMPORT_OR_EXPORT_SPECIFIERS,
                |p| Some(p.parse_import_or_export_specifier(true)),
                sk::OPEN_BRACE,
                sk::CLOSE_BRACE,
            );
        } else {
            self.parse_bracketed_list(
                pc::IMPORT_OR_EXPORT_SPECIFIERS,
                |p| Some(p.parse_import_or_export_specifier(false)),
                sk::OPEN_BRACE,
                sk::CLOSE_BRACE,
            );
        }
    }

    /// `parseModuleExportName(parseNameWithKeywordCheck)`: remembers whether the name is a reserved word.
    fn parse_module_export_name_with_keyword_check(&mut self, check_identifier_is_keyword: &mut bool) -> Node {
        if self.tok() == sk::STRING_LITERAL {
            return self.parse_literal_node();
        }
        *check_identifier_is_keyword = is_keyword(self.tok()) && !self.is_identifier();
        self.parse_identifier_name()
    }

    fn parse_import_or_export_specifier(&mut self, is_import_specifier: bool) -> Node {
        let pos = self.full_start;
        let mut check_identifier_is_keyword = is_keyword(self.tok()) && !self.is_identifier();
        let mut can_parse_as_keyword = true;
        let mut name = self.parse_module_export_name(Self::parse_identifier_name);
        let is_type_text =
            |n: &Node| matches!((n.kind, &n.data), (sk::IDENTIFIER, Data::Text(t)) if t.iter().copied().eq("type".encode_utf16()));
        if is_type_text(&name) {
            if self.tok() == sk::AS {
                let first_as = self.parse_identifier_name();
                if self.tok() == sk::AS {
                    let second_as = self.parse_identifier_name();
                    if self.can_parse_module_export_name() {
                        name = self.parse_module_export_name_with_keyword_check(&mut check_identifier_is_keyword);
                    } else {
                        name = second_as;
                    }
                    can_parse_as_keyword = false;
                } else if self.can_parse_module_export_name() {
                    can_parse_as_keyword = false;
                    name = self.parse_module_export_name_with_keyword_check(&mut check_identifier_is_keyword);
                } else {
                    name = first_as;
                }
            } else if self.can_parse_module_export_name() {
                name = self.parse_module_export_name_with_keyword_check(&mut check_identifier_is_keyword);
            }
        }
        if can_parse_as_keyword && self.tok() == sk::AS {
            self.parse_expected(sk::AS);
            name = self.parse_module_export_name_with_keyword_check(&mut check_identifier_is_keyword);
        }
        if is_import_specifier && (name.kind != sk::IDENTIFIER || check_identifier_is_keyword) {
            self.error();
        }
        self.node(N_OTHER, pos)
    }

    fn parse_export_declaration(&mut self) -> Node {
        let pos = self.full_start;
        let saved_await_context = self.in_await_context();
        self.set_context_flag(true, CTX_AWAIT);
        let mut has_module_specifier = false;
        self.parse_optional(sk::TYPE);
        if self.parse_optional(sk::ASTERISK) {
            if self.parse_optional(sk::AS) {
                self.parse_module_export_name(Self::parse_identifier_name);
            }
            self.parse_expected(sk::FROM);
            self.parse_module_specifier();
            has_module_specifier = true;
        } else {
            self.parse_named_imports_or_exports(false);
            if self.tok() == sk::FROM || (self.tok() == sk::STRING_LITERAL && !self.has_preceding_line_break()) {
                self.parse_expected(sk::FROM);
                self.parse_module_specifier();
                has_module_specifier = true;
            }
        }
        let current_token = self.tok();
        if has_module_specifier && (current_token == sk::WITH || current_token == sk::ASSERT) && !self.has_preceding_line_break() {
            self.parse_import_attributes(current_token, false);
        }
        self.parse_semicolon();
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        self.node(N_OTHER, pos)
    }

    fn parse_export_assignment(&mut self) -> Node {
        let pos = self.full_start;
        let saved_await_context = self.in_await_context();
        self.set_context_flag(true, CTX_AWAIT);
        if !self.parse_optional(sk::EQUALS) {
            self.parse_expected(sk::DEFAULT);
        }
        self.parse_assignment_expression_or_higher(true);
        self.parse_semicolon();
        self.set_context_flag(saved_await_context, CTX_AWAIT);
        self.node(N_OTHER, pos)
    }

    // ----- JSON -----

    /// `parseJsonText`: the expression of the one statement of the file, None for an empty file.
    fn parse_json_text(&mut self) -> Option<Node> {
        self.next_token();
        let pos = self.full_start;
        if self.tok() == sk::END_OF_FILE {
            return None;
        }
        let mut expressions: Vec<Node> = Vec::new();
        while self.tok() != sk::END_OF_FILE {
            let expression = match self.tok() {
                sk::OPEN_BRACKET => self.parse_array_literal_expression(),
                sk::TRUE | sk::FALSE | sk::NULL => self.parse_token_node(),
                sk::MINUS => {
                    if self.look_ahead(|p| p.next_token() == sk::NUMERIC_LITERAL && p.next_token() != sk::COLON) {
                        self.parse_prefix_unary_expression()
                    } else {
                        self.parse_object_literal_expression()
                    }
                }
                sk::NUMERIC_LITERAL | sk::STRING_LITERAL => {
                    if self.look_ahead(|p| p.next_token() != sk::COLON) {
                        self.parse_literal_node()
                    } else {
                        self.parse_object_literal_expression()
                    }
                }
                _ => self.parse_object_literal_expression(),
            };
            let first = expressions.is_empty();
            expressions.push(expression);
            if first && self.tok() != sk::END_OF_FILE {
                self.error();
            }
        }
        if expressions.len() == 1 {
            expressions.pop()
        } else {
            Some(self.node_with(sk::N_ARRAY_LITERAL_EXPRESSION, pos, Data::Elems(expressions)))
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Unicode identifier tables (TypeScript 5.9.3 `unicodeESNextIdentifierStart` / `unicodeESNextIdentifierPart`)
// ---------------------------------------------------------------------------------------------------

const ID_START: [u32; 1320] = [
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
const ID_PART: [u32; 1538] = [
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

    fn s(text: &str) -> JsonValue {
        JsonValue::String(text.to_string())
    }

    fn n(value: f64) -> JsonValue {
        JsonValue::Number(value)
    }

    fn obj(pairs: &[(&str, JsonValue)]) -> JsonValue {
        JsonValue::Object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    fn arr(items: &[JsonValue]) -> JsonValue {
        JsonValue::Array(items.to_vec())
    }

    fn keys(value: &JsonValue) -> Vec<String> {
        value.entries().into_iter().map(|(k, _)| k).collect()
    }

    #[test]
    fn reads_a_plain_tsconfig() {
        let config = parse_config_text(r#"{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}"#);
        let options = config.get("compilerOptions").unwrap();
        assert_eq!(options.get("baseUrl"), Some(&s(".")));
        assert_eq!(options.get("paths"), Some(&obj(&[("@/*", arr(&[s("src/*")]))])));
        assert_eq!(config.get("missing"), None);
    }

    #[test]
    fn comments_trailing_commas_single_quotes_and_unquoted_keys() {
        let text = "{\n // line\n \"a\": 1, /* block */ 'b': 'two', c: [1, 2,], d: {e: true,},\n}";
        let expected = obj(&[("a", n(1.0)), ("b", s("two")), ("c", arr(&[n(1.0), n(2.0)])), ("d", obj(&[("e", JsonValue::Bool(true))]))]);
        assert_eq!(parse_config_text(text), expected);
    }

    #[test]
    fn numeric_literals() {
        let text =
            "{\"a\": 0x1F, \"b\": 0b101, \"c\": 0o17, \"d\": 1_000, \"e\": .5, \"f\": 5., \"g\": 1e3, \"h\": 017, \"i\": 09, \"j\": 1E-2}";
        let config = parse_config_text(text);
        let values: Vec<JsonValue> = config.entries().into_iter().map(|(_, v)| v).collect();
        let want = [31.0, 5.0, 15.0, 1000.0, 0.5, 5.0, 1000.0, 15.0, 9.0, 0.01].map(n);
        assert_eq!(values, want);
    }

    #[test]
    fn unary_minus_and_plus() {
        let config = parse_config_text(r#"{"a": -1, "b": +1, "c": - 2, "d": --3, "e": -"x", "f": -0, "g": -0x10}"#);
        assert_eq!(config.get("a"), Some(&n(-1.0)));
        assert_eq!(config.get("b"), Some(&JsonValue::Undefined));
        assert_eq!(config.get("c"), Some(&n(-2.0)));
        assert_eq!(config.get("d"), Some(&JsonValue::Undefined));
        assert_eq!(config.get("e"), Some(&JsonValue::Undefined));
        assert!(matches!(config.get("f"), Some(JsonValue::Number(z)) if *z == 0.0 && z.is_sign_negative()));
        assert_eq!(config.get("g"), Some(&n(-16.0)));
    }

    #[test]
    fn bigint_nan_identifiers_and_calls_are_undefined() {
        let config = parse_config_text(r#"{"a": 1n, "b": NaN, "c": foo, "d": foo(1), "e": 1 + 2, "f": `t`, "g": /re/, "h": 2}"#);
        for key in ["a", "b", "c", "d", "e", "f", "g"] {
            assert_eq!(config.get(key), Some(&JsonValue::Undefined), "{key}");
        }
        assert_eq!(config.get("h"), Some(&n(2.0)));
    }

    #[test]
    fn undefined_is_kept_in_objects_and_dropped_from_arrays() {
        let config = parse_config_text(r#"{"a": [1, x, , 2, null], "b": x}"#);
        assert_eq!(config.get("a"), Some(&arr(&[n(1.0), n(2.0), JsonValue::Null])));
        assert_eq!(keys(&config), ["a", "b"]);
        assert_eq!(config.get("b"), Some(&JsonValue::Undefined));
    }

    #[test]
    fn numeric_property_names_are_normalized_and_ordered() {
        let config =
            parse_config_text(r#"{"b": 1, 1.50: 2, 0x10: 3, 1e21: 4, "a": 5, 2: 6, 1: 7, 1n: 8, [`k`]: 9, ["m"]: 10, [3]: 11, [x]: 12}"#);
        assert_eq!(keys(&config), ["1", "2", "3", "16", "b", "1.5", "1e+21", "a", "1n", "k", "m"]);
        assert_eq!(config.get("16"), Some(&n(3.0)));
        assert_eq!(config.get("1"), Some(&n(7.0)));
    }

    #[test]
    fn repeated_keys_keep_the_first_position_and_the_last_value() {
        let config = parse_config_text(r#"{"a": 1, "b": 2, "a": 3}"#);
        assert_eq!(config, obj(&[("a", n(3.0)), ("b", n(2.0))]));
        assert_eq!(keys(&config), ["a", "b"]);
    }

    #[test]
    fn the_root_must_be_an_object_or_an_array_with_one() {
        assert_eq!(parse_config_text(""), obj(&[]));
        assert_eq!(parse_config_text("   // nothing\n"), obj(&[]));
        assert_eq!(parse_config_text("1"), obj(&[]));
        assert_eq!(parse_config_text("\"s\""), obj(&[]));
        assert_eq!(parse_config_text("null"), obj(&[]));
        assert_eq!(parse_config_text("[1, 2]"), obj(&[]));
        assert_eq!(parse_config_text(r#"[1, {"a": 1}, {"b": 2}]"#), obj(&[("a", n(1.0))]));
        assert_eq!(parse_config_text(r#"[[{"a": 1}]]"#), obj(&[]));
    }

    #[test]
    fn several_top_level_values_become_an_array_and_the_first_object_wins() {
        assert_eq!(parse_config_text(r#"{"a": 1} {"b": 2}"#), obj(&[("a", n(1.0))]));
        assert_eq!(parse_config_text(r#"1 {"b": 2}"#), obj(&[("b", n(2.0))]));
        assert_eq!(parse_config_text(r#""x" "y" {"b": 2}"#), obj(&[("b", n(2.0))]));
        assert_eq!(parse_config_text(r#"{"a": 1} extra"#), obj(&[("a", n(1.0))]));
        assert_eq!(parse_config_text("1 2 3"), obj(&[]));
    }

    #[test]
    fn a_top_level_member_without_braces_is_read_as_an_object() {
        assert_eq!(parse_config_text(r#""a": 1, "b": 2"#), obj(&[("a", n(1.0)), ("b", n(2.0))]));
        // the `-` is skipped as an unexpected token; the key is the number 1
        assert_eq!(parse_config_text("-1: 2"), obj(&[("1", n(2.0))]));
        assert_eq!(parse_config_text("a: 1"), obj(&[("a", n(1.0))]));
    }

    #[test]
    fn recovers_from_garbage_the_way_typescript_does() {
        assert_eq!(parse_config_text(r#"{"a" 1}"#), obj(&[("a", n(1.0))]));
        assert_eq!(parse_config_text(r#"{"a": 1 "b": 2}"#), obj(&[("a", n(1.0)), ("b", n(2.0))]));
        assert_eq!(parse_config_text(r#"{"a": 1; "b": 2}"#), obj(&[("a", n(1.0)), ("b", n(2.0))]));
        assert_eq!(parse_config_text(r#"{"a": 1,, "b": 2}"#), obj(&[("a", n(1.0)), ("b", n(2.0))]));
        // `"y"` has no colon: a property without a value
        assert_eq!(parse_config_text(r#"{"a": "x" "y", "b": 2}"#), obj(&[("a", s("x")), ("y", JsonValue::Undefined), ("b", n(2.0))]));
        assert_eq!(parse_config_text(r#"{"a": 1"#), obj(&[("a", n(1.0))]));
        assert_eq!(parse_config_text(r#"{"a": [1, 2"#), obj(&[("a", arr(&[n(1.0), n(2.0)]))]));
        assert_eq!(parse_config_text(r#"{"a": "unterminated"#), obj(&[("a", s("unterminated"))]));
        // a string ends at the line break; `break` is a property without a value, then `", 2}` is a string
        assert_eq!(
            parse_config_text("{\"a\": \"line\nbreak\", \"b\": 2}"),
            obj(&[("a", s("line")), ("break", s(", ")), (": 2}", JsonValue::Undefined)])
        );
        assert_eq!(parse_config_text(r#"{"a": 1} /* open"#), obj(&[("a", n(1.0))]));
        assert_eq!(parse_config_text("{"), obj(&[]));
        assert_eq!(parse_config_text("}"), obj(&[]));
        assert_eq!(parse_config_text("{{{{"), obj(&[]));
    }

    #[test]
    fn javascript_values_are_parsed_as_javascript_so_the_next_property_survives() {
        let values = [
            "x => x",
            "(a, b) => { return a + b }",
            "async () => {}",
            "function () { if (a) { b() } else { c } }",
            "class A extends B { static x = 1; m() {} get g() { return 1 } #p = 2 }",
            "a ? b : c",
            "a.b.c(d)[e]",
            "new X(1)",
            "`a${b}c`",
            "/[/]\\/x/gi",
            "<a b={c}>text {d} <e/></a>",
            "<></>",
            "(x: number): string => x",
            "a as const",
            "typeof a === 'b'",
            "[1, 2].map(x => x * 2)",
            "{ nested: { deep: [1, 2] } }.nested",
            "void 0",
        ];
        for value in values {
            let config = parse_config_text(&format!(r#"{{"a": 1, "b": {value}, "c": 3}}"#));
            assert_eq!(config.get("a"), Some(&n(1.0)), "{value}");
            assert_eq!(config.get("c"), Some(&n(3.0)), "{value}");
            assert_eq!(config.get("b"), Some(&JsonValue::Undefined), "{value}");
        }
    }

    #[test]
    fn methods_accessors_spread_and_shorthand_members_are_dropped() {
        let config = parse_config_text(r#"{"a": 1, m() {}, get g() { return 1 }, set s(v) {}, async *gen() {}, ...x, y, z = 1, "b": 2}"#);
        assert_eq!(config, obj(&[("a", n(1.0)), ("b", n(2.0))]));
    }

    #[test]
    fn string_escapes() {
        let config = parse_config_text(r#"{"a": "\n\t\"\\\/A\x41\u{1F600}\0\101", "b": 'it\'s', "c": "\q"}"#);
        assert_eq!(config.get("a"), Some(&s("\n\t\"\\/AA\u{1F600}\0A")));
        assert_eq!(config.get("b"), Some(&s("it's")));
        assert_eq!(config.get("c"), Some(&s("q")));
        let config = parse_config_text("{\"a\": \"x\\\ny\"}");
        assert_eq!(config.get("a"), Some(&s("xy")));
    }

    #[test]
    fn lone_surrogates_become_the_replacement_character() {
        let config = parse_config_text(r#"{"a": "\uD800", "b": "x\uDC00y", "c": "😀"}"#);
        assert_eq!(config.get("a"), Some(&s("\u{FFFD}")));
        assert_eq!(config.get("b"), Some(&s("x\u{FFFD}y")));
        assert_eq!(config.get("c"), Some(&s("\u{1F600}")));
    }

    #[test]
    fn proto_sets_the_prototype_for_an_object_array_or_null() {
        let config = parse_config_text(r#"{"__proto__": {"compilerOptions": {"paths": {}}}, "own": 1}"#);
        assert_eq!(keys(&config), ["own"]);
        assert_eq!(config.get("compilerOptions"), Some(&obj(&[("paths", obj(&[]))])));
        assert_eq!(config.get("own"), Some(&n(1.0)));
        assert_eq!(config.get("__proto__").map(|p| p.get("compilerOptions").is_some()), Some(true));

        let config = parse_config_text(r#"{"__proto__": null, "own": 1}"#);
        assert_eq!(keys(&config), ["own"]);
        assert_eq!(config.get("__proto__"), None);

        let config = parse_config_text(r#"{"__proto__": ["x"]}"#);
        assert_eq!(keys(&config), Vec::<String>::new());
        assert_eq!(config.get("0"), Some(&s("x")));
    }

    #[test]
    fn proto_ignores_other_values_and_own_properties_win() {
        for value in ["1", "\"s\"", "true", "x", "-1"] {
            let config = parse_config_text(&format!(r#"{{"__proto__": {value}, "a": 1}}"#));
            assert_eq!(keys(&config), ["a"], "{value}");
            assert_eq!(config.get("__proto__"), None, "{value}");
        }
        let config = parse_config_text(r#"{"__proto__": {"a": 1, "b": 2}, "a": 3}"#);
        assert_eq!(config.get("a"), Some(&n(3.0)));
        assert_eq!(config.get("b"), Some(&n(2.0)));
        assert_eq!(keys(&config), ["a"]);
    }

    #[test]
    fn proto_chains_and_a_proto_property_after_a_null_prototype() {
        let config = parse_config_text(r#"{"__proto__": {"__proto__": {"deep": 1}}}"#);
        assert_eq!(config.get("deep"), Some(&n(1.0)));
        // once the prototype is null there is no setter left: `__proto__` is an ordinary property
        let config = parse_config_text(r#"{"__proto__": null, "__proto__": {"x": 1}, "y": 2}"#);
        assert_eq!(keys(&config), ["__proto__", "y"]);
        assert_eq!(config.get("x"), None);
        assert_eq!(config.get("__proto__"), Some(&obj(&[("x", n(1.0))])));
        // a later `__proto__` assignment replaces the prototype
        let config = parse_config_text(r#"{"__proto__": {"x": 1}, "__proto__": {"y": 2}}"#);
        assert_eq!(config.get("x"), None);
        assert_eq!(config.get("y"), Some(&n(2.0)));
    }

    #[test]
    fn get_and_entries_follow_javascript_property_access() {
        let config = parse_config_text(r#"{"b": 1, "10": 2, "2": 3, "a": 4, "01": 5}"#);
        assert_eq!(keys(&config), ["2", "10", "b", "a", "01"]);
        assert_eq!(config.get("a"), Some(&n(4.0)));
        assert_eq!(arr(&[n(1.0)]).get("0"), None);
        assert_eq!(n(1.0).get("a"), None);
        assert!(JsonValue::Object(IndexMap::new()).truthy());
        assert!(!JsonValue::String(String::new()).truthy());
        assert!(!n(f64::NAN).truthy());
        assert_eq!(s("ab").entries().len(), 2);
        assert_eq!(JsonValue::Number(1.5).received(), "Received type number (1.5)");
        assert_eq!(JsonValue::Undefined.received(), "Received undefined");
    }

    #[test]
    fn decoding_follows_ts_sys_read_file() {
        let doc = r#"{"a": "é😀"}"#;
        let expected = obj(&[("a", s("é😀"))]);
        let utf16 = |big_endian: bool| -> Vec<u8> {
            let mut out = if big_endian { vec![0xFE, 0xFF] } else { vec![0xFF, 0xFE] };
            for unit in doc.encode_utf16() {
                out.extend(if big_endian { unit.to_be_bytes() } else { unit.to_le_bytes() });
            }
            out
        };
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend(doc.as_bytes());
        let mut odd = utf16(false);
        odd.push(0x41);
        for bytes in [doc.as_bytes().to_vec(), bom, utf16(false), utf16(true), odd] {
            assert_eq!(try_parse_units_from_bytes(&bytes), expected, "{bytes:?}");
        }
        // no BOM: UTF-16 is not detected, the bytes are read as (invalid) UTF-8
        assert_eq!(try_parse_units_from_bytes(&[0x7B, 0x00, 0x7D, 0x00]), obj(&[]));
        // invalid UTF-8 becomes U+FFFD, a BOM-only or empty file is empty
        assert_eq!(try_parse_units_from_bytes(b"{\"a\": \"\xFF\"}"), obj(&[("a", s("\u{FFFD}"))]));
        assert_eq!(try_parse_units_from_bytes(&[0xEF, 0xBB, 0xBF]), obj(&[]));
        assert_eq!(try_parse_units_from_bytes(&[0xFF, 0xFE]), obj(&[]));
        assert_eq!(try_parse_units_from_bytes(b""), obj(&[]));
        // a second BOM is just whitespace
        assert_eq!(try_parse_units_from_bytes(&[0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF, b'{', b'}']), obj(&[]));
        assert_eq!(try_parse_units_from_bytes("\u{FEFF}{\"a\": 1}".as_bytes()), obj(&[("a", n(1.0))]));
    }

    fn try_parse_units_from_bytes(bytes: &[u8]) -> JsonValue {
        parse_units(&decode_file(bytes)).unwrap()
    }

    #[test]
    fn reads_files_and_treats_unreadable_ones_as_empty() {
        let dir = std::env::temp_dir().join(format!("jsonc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tsconfig.json");
        std::fs::write(&file, r#"{"compilerOptions": {"baseUrl": "src"}} // x"#).unwrap();
        let config = read_config_file(file.to_str().unwrap());
        assert_eq!(config.get("compilerOptions").and_then(|o| o.get("baseUrl")), Some(&s("src")));
        assert_eq!(read_config_file(dir.join("missing.json").to_str().unwrap()), obj(&[]));
        assert_eq!(read_config_file(dir.to_str().unwrap()), obj(&[]));
        assert_eq!(try_read_config_file(dir.to_str().unwrap()), Ok(obj(&[])));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn typescript_exceptions_are_errs_and_empty_objects() {
        // `Debug.assert(isAwaitUsingDeclaration())` fails in parseVariableDeclarationList
        let text = r#"{"compilerOptions": {private <= using < ~ ${ constructor @d await Infinity in null if é accessor out </ undefined ) accessor . class A {} boolean set 08 declare 08}, NaN "\0" => -= "é"    <a/> NaN keyof public null ??= "\1" ++ function () {} a 017 * [] void new X() global ( ""}"#;
        assert_eq!(try_parse_config_text(text), Err("Debug Failure. False expression.".to_string()));
        assert_eq!(parse_config_text(text), obj(&[]));
    }

    #[test]
    fn deep_nesting_is_an_error_instead_of_a_crash() {
        let deep = "[".repeat(200_000);
        assert_eq!(try_parse_config_text(&deep), Err("Maximum call stack size exceeded".to_string()));
        assert_eq!(parse_config_text(&format!("{{\"a\": {}}}", "{\"a\": ".repeat(100_000))), obj(&[]));
        let ok = format!("{{\"a\": {}{}}}", "[".repeat(500), "]".repeat(500));
        assert!(matches!(parse_config_text(&ok).get("a"), Some(JsonValue::Array(_))));
    }

    #[test]
    fn jsx_and_type_syntax_inside_values() {
        let config = parse_config_text(r#"{"a": <div a="1" b={2}>{x}<br/></div>, "b": <T,>(x: T) => x, "c": 3}"#);
        assert_eq!(config.get("c"), Some(&n(3.0)));
        let config = parse_config_text(r#"{"a": function f<T extends U = V>(this: W, x?: X, ...r: Y[]): asserts x is Z {}, "c": 3}"#);
        assert_eq!(config.get("c"), Some(&n(3.0)));
    }

    #[test]
    fn conflict_markers_shebang_and_unicode_identifiers() {
        let config = parse_config_text("#!/usr/bin/env node\n{\"a\": 1}");
        assert_eq!(config.get("a"), Some(&n(1.0)));
        let config = parse_config_text("{\"a\": 1,\n<<<<<<< HEAD\n\"b\": 2,\n=======\n\"b\": 3,\n>>>>>>> branch\n\"c\": 4}");
        assert_eq!(config.get("a"), Some(&n(1.0)));
        assert_eq!(config.get("c"), Some(&n(4.0)));
        let config = parse_config_text("{ día: 1, 日本: 2, \\u0061b: 3, \\u{63}d: 4 }");
        assert_eq!(config, obj(&[("día", n(1.0)), ("日本", n(2.0)), ("ab", n(3.0)), ("cd", n(4.0))]));
    }
}
