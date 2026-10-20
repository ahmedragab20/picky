//! Replays a repo's recent history through picky's decision and records how many test files
//! each change selects. Graph only (model: none): no API key, and the numbers are the floor
//! picky always runs; with Jev, tests outside the graph can be added on top.
//!
//!   cargo run --release --example selection -- <repo-dir> <name> [commits=100]
//!
//! Each first-parent commit on the checked-out branch is treated as one PR: its first parent is
//! the base. The repo is checked out at every commit, so use a throwaway clone. Results are merged
//! into bench/results.json under <name>.

#[path = "common.rs"]
mod common;

use common::{decide_at, git, num, repo_url, today};
use picky::decide::TEST_GLOBS;
use picky::glob::Glob;
use picky::graph::is_vendored;
use serde_json::{Map, Value, json};

struct Run {
    sha: String,
    subject: String,
    changed: usize,
    tests: usize,
    run: usize,
    kind: &'static str,
    reason: String,
    ms: u64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(dir), Some(name)) = (args.first(), args.get(1)) else {
        eprintln!("usage: cargo run --release --example selection -- <repo-dir> <name> [commits=100]");
        std::process::exit(2);
    };
    let count: usize = args.get(2).map_or(100, |c| c.parse().expect("commits is a number"));
    let root = picky::npath::resolve(&[dir]);
    let is_test = Glob::new(TEST_GLOBS).expect("valid globs");

    let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let listed = git(&root, &["rev-list", "--first-parent", "-n", &(count + 1).to_string(), "HEAD"]);
    let shas: Vec<&str> = listed.split('\n').take(count).collect();
    let mut runs: Vec<Run> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for (i, sha) in shas.iter().enumerate() {
            git(&root, &["checkout", "-q", "--detach", "--force", sha]);
            let tests = git(&root, &["ls-files"]).split('\n').filter(|f| is_test.is_match(f) && !is_vendored(f)).count();
            let d = decide_at(&root, sha);
            let first_line = d.reason.split('\n').next().unwrap_or("").to_string();
            // A failed git call (e.g. a lazy blob fetch over a flaky network) is the harness failing, not picky deciding.
            if d.reason.starts_with("internal error") {
                errors.push(json!({ "sha": &sha[..10], "reason": first_line }));
                eprintln!("{name} {}/{} {}: excluded ({first_line})", i + 1, shas.len(), &sha[..10]);
                continue;
            }
            let run = if d.full { tests } else { d.tests.values().filter(|t| t.run).count() };
            let kind = if run == 0 {
                "none"
            } else if run == tests {
                "all"
            } else {
                "narrowed"
            };
            eprintln!("{name} {}/{} {}: {run}/{tests} ({} ms) {}", i + 1, shas.len(), &sha[..10], d.ms, d.reason);
            runs.push(Run {
                sha: sha[..10].to_string(),
                subject: git(&root, &["log", "-1", "--format=%s", sha]),
                changed: d.changed.len(),
                tests,
                run,
                kind,
                reason: d.reason,
                ms: d.ms,
            });
        }
    }));
    git(&root, &["checkout", "-q", "--force", &branch]);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    if runs.is_empty() {
        eprintln!("no commit could be measured ({} excluded)", errors.len());
        std::process::exit(1);
    }

    let pct = |xs: &[f64], p: f64| xs[((p / 100.0) * xs.len() as f64).floor().min(xs.len() as f64 - 1.0) as usize];
    let share = |k: &str| runs.iter().filter(|r| r.kind == k).count() as f64 / runs.len() as f64;
    let mut fractions: Vec<f64> = runs.iter().map(|r| if r.tests > 0 { r.run as f64 / r.tests as f64 } else { 0.0 }).collect();
    fractions.sort_by(f64::total_cmp);
    let mut ms: Vec<f64> = runs.iter().map(|r| r.ms as f64).collect();
    ms.sort_by(f64::total_cmp);
    let summary = json!({
        "repo": repo_url(&root),
        "head": git(&root, &["rev-parse", "--short=10", "HEAD"]),
        "commits": runs.len(),
        "excluded": errors,
        "testFiles": runs.first().map_or(0, |r| r.tests),
        "medianSelected": num(pct(&fractions, 50.0)),
        "meanSelected": num(fractions.iter().sum::<f64>() / fractions.len() as f64),
        "shareNone": num(share("none")),
        "shareNarrowed": num(share("narrowed")),
        "shareAll": num(share("all")),
        "decisionMsP50": num(pct(&ms, 50.0)),
        "decisionMsP95": num(pct(&ms, 95.0)),
        "measured": today(),
        "runs": runs.iter().map(|r| json!({
            "sha": r.sha,
            "subject": r.subject,
            "changed": r.changed,
            "tests": r.tests,
            "run": r.run,
            "kind": r.kind,
            "reason": r.reason,
            "ms": r.ms,
        })).collect::<Vec<_>>(),
    });

    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/bench/results.json");
    let mut all: Map<String, Value> = std::fs::read_to_string(out).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    all.insert(name.clone(), summary.clone());
    std::fs::write(out, format!("{}\n", serde_json::to_string_pretty(&all).unwrap())).expect("write results.json");
    let mut brief = summary;
    brief.as_object_mut().unwrap().shift_remove("runs");
    println!("{}", serde_json::to_string_pretty(&brief).unwrap());
}
