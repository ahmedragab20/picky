//! Records which test files picky's graph runs, and why, for a few real commits of one repo.
//! The docs landing page draws it as a grid with one cell per test file.
//!
//!   cargo run --release --example hero -- <repo-dir> <sha>...
//!
//! Like selection.rs: graph only (model: none), each commit's first parent is the base, and the
//! repo is checked out at every commit, so use a throwaway clone. Writes bench/hero.json.

#[path = "common.rs"]
mod common;

use common::{decide_at, git, repo_url};
use indexmap::IndexSet;
use serde_json::{Map, Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((dir, shas)) = args.split_first().filter(|(_, s)| !s.is_empty()) else {
        eprintln!("usage: cargo run --release --example hero -- <repo-dir> <sha>...");
        std::process::exit(2);
    };
    let root = picky::npath::resolve(&[dir]);
    let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let mut commits = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for sha in shas {
            git(&root, &["checkout", "-q", "--detach", "--force", sha]);
            let d = decide_at(&root, sha);
            if d.reason.starts_with("internal error") {
                panic!("{sha}: {}", d.reason);
            }
            // Why everything (or one ecosystem) runs, when a gate decided it.
            let gate = if d.full {
                Value::String(d.reason.clone())
            } else {
                d.ecosystems.as_ref().and_then(|e| e.values().next()).map_or(Value::Null, |r| Value::String(r.clone()))
            };
            commits.push((git(&root, &["rev-parse", "--short=7", sha]), git(&root, &["log", "-1", "--format=%s", sha]), d, gate));
        }
    }));
    git(&root, &["checkout", "-q", "--force", &branch]);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }

    // One cell per test file seen in any of the commits, in path order.
    let mut files: Vec<String> =
        commits.iter().flat_map(|(_, _, d, _)| d.tests.keys().cloned()).collect::<IndexSet<_>>().into_iter().collect();
    picky::js::sort_strings(&mut files);
    let out = json!({
        "repo": repo_url(&root),
        "files": files,
        "commits": commits.iter().map(|(sha, subject, d, gate)| {
            // Index into `files` -> why it runs. A file missing here was skipped.
            let run: Map<String, Value> = files
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    let why = if d.full { Some(d.reason.clone()) } else { d.tests.get(f).filter(|t| t.run).map(|t| t.reason.clone()) };
                    why.map(|w| (i.to_string(), Value::String(w)))
                })
                .collect();
            json!({
                "sha": sha,
                "subject": subject,
                "changed": d.changed.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
                "gate": gate,
                "run": run,
            })
        }).collect::<Vec<_>>(),
    });
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/bench/hero.json");
    std::fs::write(path, format!("{}\n", serde_json::to_string(&out).unwrap())).expect("write hero.json");
    for c in out["commits"].as_array().unwrap() {
        println!(
            "{} {}/{} {}",
            c["sha"].as_str().unwrap(),
            c["run"].as_object().unwrap().len(),
            files.len(),
            c["subject"].as_str().unwrap()
        );
    }
}
