use std::sync::{Arc, mpsc};
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::Value;

use crate::js;

#[derive(Clone, Debug)]
pub struct Question {
    pub id: String,
    pub instructions: String,
}

/// A response: its status and body text (Err when reading the body failed, with the message).
pub struct Response {
    pub status: u16,
    pub body: Result<String, String>,
}

/// The HTTP POST picky makes. Err is the message of a request that failed to complete.
pub trait Fetch: Send + Sync {
    fn post(&self, url: &str, headers: &[(&str, String)], body: &str, timeout: Duration) -> Result<Response, String>;
}

pub struct JudgeOptions {
    pub api_key: String,
    pub model: String,
    /// Epoch ms after which no new attempt starts.
    pub deadline: f64,
    pub fetch: Arc<dyn Fetch>,
    pub endpoint: Option<String>,
    pub batch_size: usize,
}

pub struct JudgeResult {
    /// p(yes) per question id; ids missing here were not answered.
    pub answers: IndexMap<String, f64>,
    pub requests: usize,
    pub input_tokens: f64,
}

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const RETRYABLE: &[u16] = &[429, 500, 502, 503, 504, 529];

pub fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_millis() as f64)
}

/// Asks Jev one yes/no question per candidate against a shared state. Batches run in parallel;
/// the first batch to fail fails the call.
pub fn ask_jev(state: &str, questions: &[Question], opts: &JudgeOptions) -> Result<JudgeResult, String> {
    let size = opts.batch_size.max(1);
    let batches: Vec<Vec<Question>> = questions.chunks(size).map(<[Question]>::to_vec).collect();
    let state: Arc<str> = Arc::from(state);
    let (tx, rx) = mpsc::channel();
    for (i, batch) in batches.iter().enumerate() {
        let tx = tx.clone();
        let state = Arc::clone(&state);
        let batch = batch.clone();
        let opts = JudgeOptions {
            api_key: opts.api_key.clone(),
            model: opts.model.clone(),
            deadline: opts.deadline,
            fetch: Arc::clone(&opts.fetch),
            endpoint: opts.endpoint.clone(),
            batch_size: size,
        };
        std::thread::spawn(move || {
            let _ = tx.send((i, ask_batch(&state, &batch, &opts)));
        });
    }
    drop(tx);
    let mut results: Vec<Option<(IndexMap<String, f64>, f64)>> = vec![None; batches.len()];
    for (i, result) in rx {
        results[i] = Some(result?);
    }
    let mut answers = IndexMap::new();
    let mut input_tokens = 0.0;
    for (a, tokens) in results.into_iter().flatten() {
        answers.extend(a);
        input_tokens += tokens;
    }
    Ok(JudgeResult { answers, requests: batches.len(), input_tokens })
}

fn ask_batch(state: &str, batch: &[Question], opts: &JudgeOptions) -> Result<(IndexMap<String, f64>, f64), String> {
    let mut questions = serde_json::Map::new();
    for q in batch {
        questions.insert(
            q.id.clone(),
            serde_json::json!({
                "type": "noul",
                "instructions": q.instructions,
                "criteria": {
                    "true": "The changes could plausibly change the outcome of these tests.",
                    "false": "The changes clearly cannot affect these tests.",
                },
            }),
        );
    }
    let body =
        serde_json::to_string(&serde_json::json!({ "model": opts.model, "state": state, "questions": questions })).expect("serializable");
    let headers = [("authorization", format!("Bearer {}", opts.api_key)), ("content-type", "application/json".to_string())];
    let url = opts.endpoint.as_deref().unwrap_or(ENDPOINT);

    let mut attempt: i32 = 0;
    loop {
        let remaining = opts.deadline - now_ms();
        if remaining <= 0.0 {
            return Err("time budget exhausted".into());
        }
        let timeout = abort_timeout(remaining).map_err(|e| format!("request failed: {e}"))?;
        let res = opts.fetch.post(url, &headers, &body, timeout).map_err(|e| format!("request failed: {e}"))?;

        if (200..300).contains(&res.status) {
            let text = res.body?;
            return parse_answers(&text, batch);
        }
        if !RETRYABLE.contains(&res.status) {
            let text = res.body.unwrap_or_default();
            let detail: Vec<u16> = text.encode_utf16().take(200).collect();
            let detail = String::from_utf16_lossy(&detail);
            return Err(if detail.is_empty() { format!("HTTP {}", res.status) } else { format!("HTTP {}: {detail}", res.status) });
        }
        let backoff = 8000f64.min(500.0 * 2f64.powi(attempt));
        if now_ms() + backoff >= opts.deadline {
            return Err(format!("HTTP {}, out of time to retry", res.status));
        }
        std::thread::sleep(Duration::from_millis(backoff as u64));
        attempt += 1;
    }
}

/// `AbortSignal.timeout(ms)`: Node's validation errors, and its 1 ms fallback for delays past 2^31 - 1.
fn abort_timeout(ms: f64) -> Result<Duration, String> {
    if ms.fract() != 0.0 || !ms.is_finite() {
        return Err(out_of_range("an integer", ms));
    }
    if !(0.0..=4_294_967_295.0).contains(&ms) {
        return Err(out_of_range(">= 0 && <= 4294967295", ms));
    }
    Ok(Duration::from_millis(if ms > 2_147_483_647.0 { 1 } else { ms as u64 }))
}

fn out_of_range(range: &str, value: f64) -> String {
    let received = if value.fract() == 0.0 && value.abs() > 4_294_967_296.0 {
        add_numerical_separator(&js::number_to_string(value))
    } else if value == 0.0 && value.is_sign_negative() {
        "-0".into()
    } else {
        js::number_to_string(value)
    };
    format!("The value of \"delay\" is out of range. It must be {range}. Received {received}")
}

fn add_numerical_separator(val: &str) -> String {
    let b = val.as_bytes();
    let start = usize::from(b.first() == Some(&b'-'));
    let mut res = String::new();
    let mut i = b.len();
    while i >= start + 4 {
        res = format!("_{}{res}", &val[i - 3..i]);
        i -= 3;
    }
    format!("{}{res}", &val[..i])
}

fn parse_answers(text: &str, batch: &[Question]) -> Result<(IndexMap<String, f64>, f64), String> {
    let json: Value = crate::json::parse(text).map_err(|_| json_error(text))?;
    let answers = match json.get("answers") {
        Some(a @ (Value::Object(_) | Value::Array(_))) => a,
        _ => return Err("response has no answers".into()),
    };
    let mut out = IndexMap::new();
    for q in batch {
        let p = crate::json::get(answers, &q.id).and_then(|a| crate::json::get(a, "noul")).and_then(Value::as_f64);
        if let Some(p) = p.filter(|p| p.is_finite() && (0.0..=1.0).contains(p)) {
            out.insert(q.id.clone(), p);
        }
    }
    let tokens = json.get("usage").and_then(|u| u.get("input_tokens")).and_then(Value::as_f64).unwrap_or(0.0);
    Ok((out, tokens))
}

/// The gist of V8's `JSON.parse` error, for a body that isn't JSON.
fn json_error(text: &str) -> String {
    match js::trim(text).chars().next() {
        None => "Unexpected end of JSON input".into(),
        Some(c) => format!("Unexpected token '{c}', \"{}\" is not valid JSON", text.chars().take(10).collect::<String>()),
    }
}

/// The real HTTP client: ureq with rustls.
pub struct Http;

impl Fetch for Http {
    fn post(&self, url: &str, headers: &[(&str, String)], body: &str, timeout: Duration) -> Result<Response, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into();
        let mut req = agent.post(url);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let mut res = req.send(body).map_err(|e| fetch_error(&e))?;
        let status = res.status().as_u16();
        let body = res.body_mut().with_config().limit(u64::MAX).read_to_string().map_err(|e| fetch_error(&e));
        Ok(Response { status, body })
    }
}

/// What Node's fetch says: a timeout aborts; anything else is a TypeError "fetch failed".
fn fetch_error(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "The operation was aborted due to timeout".into(),
        _ => "fetch failed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_timeouts_like_abort_signal() {
        assert_eq!(abort_timeout(1500.0), Ok(Duration::from_millis(1500)));
        assert_eq!(abort_timeout(3e9), Ok(Duration::from_millis(1)));
        assert_eq!(abort_timeout(0.5).unwrap_err(), "The value of \"delay\" is out of range. It must be an integer. Received 0.5");
        assert_eq!(abort_timeout(f64::NAN).unwrap_err(), "The value of \"delay\" is out of range. It must be an integer. Received NaN");
        assert_eq!(
            abort_timeout(9007199254740992.0).unwrap_err(),
            "The value of \"delay\" is out of range. It must be >= 0 && <= 4294967295. Received 9_007_199_254_740_992"
        );
    }
}
