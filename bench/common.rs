//! Shared by the benchmark scripts: git, the graph-only decision, JSON numbers and dates.
#![allow(dead_code)]

use std::process::Command;

use picky::Env;
use picky::decide::{DecideOptions, Decision, decide};

pub fn git(root: &str, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().expect("git");
    if !out.status.success() {
        panic!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The decision for `sha` against its first parent: graph only, enforcing, no cache.
pub fn decide_at(root: &str, sha: &str) -> Decision {
    let mut env = Env::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".into(), path);
    }
    env.insert("PICKY_MODEL".into(), "none".into());
    env.insert("PICKY_MODE".into(), "enforce".into());
    env.insert("PICKY_BASE".into(), format!("{sha}^1"));
    decide(&DecideOptions { root: root.to_string(), env, fetch: None })
}

/// The origin URL without `.git`.
pub fn repo_url(root: &str) -> String {
    let url = git(root, &["remote", "get-url", "origin"]);
    url.strip_suffix(".git").map(str::to_string).unwrap_or(url)
}

/// A number as JSON.stringify writes it: integers without a fraction.
pub fn num(x: f64) -> serde_json::Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9e15 { serde_json::json!(x as i64) } else { serde_json::json!(x) }
}

/// Today in UTC, as `YYYY-MM-DD`.
pub fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    // Howard Hinnant's civil_from_days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}
