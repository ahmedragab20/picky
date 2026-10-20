use crate::Env;
use crate::yaml::{self, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Shadow,
    Enforce,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Shadow => "shadow",
            Mode::Enforce => "enforce",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Typesafe,
    /// Graph-only: no model call, nothing outside the graph is selected.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRef {
    pub provider: Provider,
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: Mode,
    pub model: ModelRef,
    pub critical: Vec<String>,
    pub low: Vec<String>,
    pub api_key: Option<String>,
    /// Set when the config can't be honored safely; the run becomes a full run.
    pub problem: Option<String>,
    /// Raw config text, part of the decision cache key.
    pub source: String,
}

pub const DEFAULT_JEV: &str = "jev-1.14.0";
const DEFAULT_LOW: &[&str] = &["**/*.md", "**/*.mdx", "docs/**", "LICENSE*"];
const KNOWN_KEYS: &[&str] = &["extends", "mode", "model", "critical", "low"];

pub fn load_config(root: &str, env: &Env) -> Config {
    let mut source = String::new();
    let mut raw = Value::Object(Vec::new());
    let mut problem: Option<String> = None;
    let file = crate::npath::join(&[root, ".picky.yml"]);
    match std::fs::read(&file) {
        Ok(bytes) => {
            source = String::from_utf8_lossy(&bytes).into_owned();
            match yaml::parse(&source) {
                Ok(Value::Null) => {}
                Ok(v) => raw = v,
                Err(e) => problem = Some(format!(".picky.yml is invalid: {}", e.message)),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => problem = Some(format!(".picky.yml is invalid: {}", node_fs_error(&e, "open", &file))),
    }

    for key in js_keys(&raw) {
        if !KNOWN_KEYS.contains(&key.as_str()) && problem.is_none() {
            problem = Some(format!(".picky.yml has unknown key \"{key}\""));
        }
    }
    // Ignoring an org preset could drop its critical rules, so it forces a full run until supported.
    if field(&raw, "extends").is_some_and(truthy) && problem.is_none() {
        problem = Some("`extends` is not supported in this version yet".into());
    }

    let mode_value = match env.get("PICKY_MODE") {
        Some(m) => Value::String(m.clone()),
        None => field(&raw, "mode").filter(|v| !matches!(v, Value::Null)).cloned().unwrap_or(Value::String("shadow".into())),
    };
    let mode = js_string(&mode_value);
    if mode != "shadow" && mode != "enforce" && problem.is_none() {
        problem = Some(format!("unknown mode \"{mode}\""));
    }

    let model_id = match env.get("PICKY_MODEL") {
        Some(m) => m.clone(),
        None => field(&raw, "model").filter(|v| !matches!(v, Value::Null)).map_or_else(|| "jev".to_string(), js_string),
    };
    let model = parse_model(&model_id);
    if model.is_none() && problem.is_none() {
        problem = Some(format!("model \"{model_id}\" is not supported in this version (use jev, typesafe/<jev-version> or none)"));
    }

    let resolved = model.unwrap_or(ModelRef { provider: Provider::None, name: "none".into(), id: model_id.clone() });
    let api_key = if resolved.provider == Provider::Typesafe {
        env.get("PICKY_API_KEY").or_else(|| env.get("TYPESAFE_API_KEY")).cloned()
    } else {
        None
    };
    // Git may check the file out with CRLF (Windows autocrlf); the line endings don't change the config.
    let key = serde_json::json!({
        "source": source.replace("\r\n", "\n"),
        "mode": to_json(&mode_value),
        "modelId": model_id,
    });
    Config {
        mode: if mode == "enforce" { Mode::Enforce } else { Mode::Shadow },
        model: resolved,
        critical: list(field(&raw, "critical")),
        low: match field(&raw, "low") {
            None => DEFAULT_LOW.iter().map(|s| s.to_string()).collect(),
            Some(v) => list(Some(v)),
        },
        api_key,
        problem,
        source: serde_json::to_string(&key).expect("serializable"),
    }
}

pub fn parse_model(id: &str) -> Option<ModelRef> {
    if id == "none" {
        return Some(ModelRef { provider: Provider::None, name: "none".into(), id: id.into() });
    }
    if id == "jev" {
        return Some(ModelRef { provider: Provider::Typesafe, name: DEFAULT_JEV.into(), id: id.into() });
    }
    // /^typesafe\/(jev-[\w.-]+)$/
    let name = id.strip_prefix("typesafe/")?;
    let ok = name.len() > 4
        && name.starts_with("jev-")
        && name[4..].bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-');
    ok.then(|| ModelRef { provider: Provider::Typesafe, name: name.into(), id: id.into() })
}

/// `raw[key]` on the parsed config: only a mapping has fields.
fn field<'a>(raw: &'a Value, key: &str) -> Option<&'a Value> {
    match raw {
        Value::Object(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// `Object.keys(raw)`: integer keys first, ascending, then the rest in insertion order. A string
/// has its indices as keys, and so does an array.
fn js_keys(raw: &Value) -> Vec<String> {
    match raw {
        Value::Object(entries) => {
            let mut seen: Vec<String> = Vec::new();
            for (k, _) in entries {
                if !seen.contains(k) {
                    seen.push(k.clone());
                }
            }
            let index = |k: &str| k.parse::<u32>().ok().filter(|n| n.to_string() == k && *n != u32::MAX);
            let mut ints: Vec<(u32, String)> = seen.iter().filter_map(|k| index(k).map(|n| (n, k.clone()))).collect();
            ints.sort();
            let mut out: Vec<String> = ints.into_iter().map(|(_, k)| k).collect();
            out.extend(seen.into_iter().filter(|k| index(k).is_none()));
            out
        }
        Value::String(s) => (0..s.encode_utf16().count()).map(|i| i.to_string()).collect(),
        Value::Array(items) => (0..items.len()).map(|i| i.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => *n != 0.0 && !n.is_nan(),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `String(value)`.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => crate::js::number_to_string(*n),
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            items.iter().map(|i| if matches!(i, Value::Null) { String::new() } else { js_string(i) }).collect::<Vec<_>>().join(",")
        }
        Value::Object(_) => "[object Object]".into(),
    }
}

fn to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => serde_json::Number::from_f64(*n).map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(s) => s.clone().into(),
        Value::Array(items) => items.iter().map(to_json).collect(),
        Value::Object(entries) => {
            let mut map = serde_json::Map::new();
            for (k, v) in entries {
                map.insert(k.clone(), to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn list(value: Option<&Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(js_string).collect(),
        Some(v) => vec![js_string(v)],
    }
}

/// The message Node's `readFileSync` throws, e.g. `EISDIR: illegal operation on a directory, read`.
pub fn node_fs_error(e: &std::io::Error, syscall: &str, path: &str) -> String {
    match e.raw_os_error() {
        Some(21) => "EISDIR: illegal operation on a directory, read".into(),
        Some(13) => format!("EACCES: permission denied, {syscall} '{path}'"),
        Some(20) => format!("ENOTDIR: not a directory, {syscall} '{path}'"),
        _ => format!("{e}, {syscall} '{path}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_ids() {
        assert_eq!(parse_model("jev").unwrap().name, DEFAULT_JEV);
        assert_eq!(parse_model("typesafe/jev-1.2.3").unwrap().name, "jev-1.2.3");
        assert!(parse_model("typesafe/jev-").is_none());
        assert!(parse_model("typesafe/gpt").is_none());
        assert_eq!(parse_model("none").unwrap().provider, Provider::None);
    }

    #[test]
    fn orders_keys_like_object_keys() {
        let raw = Value::Object(vec![
            ("b".into(), Value::Null),
            ("2".into(), Value::Null),
            ("a".into(), Value::Null),
            ("1".into(), Value::Null),
            ("01".into(), Value::Null),
        ]);
        assert_eq!(js_keys(&raw), ["1", "2", "b", "a", "01"]);
    }
}
