//! `JSON.parse` and JavaScript-style property reads on parsed JSON.

use serde_json::Value;

/// `JSON.parse(text)`. Like JavaScript, a byte order mark is a syntax error.
pub fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// `JSON.parse(readFileSync(file, 'utf8'))`, or None on any error.
pub fn read(file: &str) -> Option<Value> {
    let bytes = std::fs::read(file).ok()?;
    parse(&String::from_utf8_lossy(&bytes)).ok()
}

/// `value[key]` on a JSON value: object properties, array indices and `length`, string units.
/// Prototype members (`toString`, ...) are never strings, numbers or plain data, so they read as None.
pub fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map.get(key),
        Value::Array(items) => canonical_index(key).and_then(|i| items.get(i)),
        _ => None,
    }
}

/// `typeof value[key] === 'string'`, also for a string's units and an array's elements.
pub fn get_str(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::String(s) => {
            let i = canonical_index(key)?;
            let unit = *s.encode_utf16().collect::<Vec<_>>().get(i)?;
            Some(String::from_utf16_lossy(&[unit]))
        }
        _ => get(value, key).and_then(Value::as_str).map(str::to_string),
    }
}

/// A key that indexes an array: a canonical non-negative integer below 2^32 - 1.
fn canonical_index(key: &str) -> Option<usize> {
    let n: u32 = key.parse().ok()?;
    (n.to_string() == key && n != u32::MAX).then_some(n as usize)
}

/// JavaScript truthiness of a JSON value.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `String(value)` for a JSON value.
pub fn to_js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::js::number_to_string(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            items.iter().map(|v| if v.is_null() { String::new() } else { to_js_string(v) }).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) => "[object Object]".into(),
    }
}
