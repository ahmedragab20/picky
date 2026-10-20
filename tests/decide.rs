mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use picky::decide::{DecideOptions, Decision, THRESHOLD, decide, decide_cached};
use picky::judge::{Fetch, Response};
use serde_json::{Value, json};

fn jev() -> Files {
    files(&[(".picky.yml", "mode: enforce\nmodel: jev\n")])
}

fn opts(root: &str, env: picky::Env, fetch: Option<Arc<dyn Fetch>>) -> DecideOptions {
    DecideOptions { root: root.to_string(), env, fetch }
}

fn runs(d: &Decision) -> Vec<String> {
    sorted(d.tests.iter().filter(|(_, t)| t.run).map(|(f, _)| f.clone()))
}

fn jev_reply(answers: &[(&str, f64)]) -> Response {
    let answers: serde_json::Map<String, Value> =
        answers.iter().map(|(k, p)| (k.to_string(), json!({ "type": "noul", "noul": p }))).collect();
    let body = json!({ "model": "jev-1.14.0", "answers": answers, "usage": { "input_tokens": 321, "output_tokens": 0 } });
    Response { status: 200, body: Ok(body.to_string()) }
}

struct Call {
    headers: Vec<(String, String)>,
    body: Value,
}

type Reply = Box<dyn Fn(usize, &Value) -> Response + Send + Sync>;

/// A fetch that records every call and answers with `reply(call index, request body)`.
struct Mock {
    calls: Mutex<Vec<Call>>,
    reply: Reply,
}

impl Mock {
    fn new(reply: impl Fn(usize, &Value) -> Response + Send + Sync + 'static) -> Arc<Mock> {
        Arc::new(Mock { calls: Mutex::new(Vec::new()), reply: Box::new(reply) })
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl Fetch for Mock {
    fn post(&self, _url: &str, headers: &[(&str, String)], body: &str, _timeout: Duration) -> Result<Response, String> {
        let body: Value = serde_json::from_str(body).unwrap();
        let mut calls = self.calls.lock().unwrap();
        let res = (self.reply)(calls.len(), &body);
        calls.push(Call { headers: headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), body });
        Ok(res)
    }
}

fn math_edit() -> Files {
    files(&[("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")])
}

#[test]
fn selects_tests_that_import_the_changed_file_directly_or_transitively() {
    let root = repo(&cat(&[&shop(), &graph_only()]), &math_edit());
    let d = decide(&opts(&root, clean_env(&[]), None));
    assert!(!d.full);
    assert_eq!(runs(&d), ["test/cart.test.ts", "test/math.test.ts"]);
    assert_eq!(d.tests["test/cart.test.ts"].reason, "imports src/math.ts");
}

#[test]
fn skips_everything_when_only_low_risk_files_changed() {
    let root = repo(&cat(&[&shop(), &graph_only()]), &files(&[("README.md", "# shop, documented\n")]));
    assert!(runs(&decide(&opts(&root, clean_env(&[]), None))).is_empty());
}

#[test]
fn runs_tests_that_still_import_a_deleted_file() {
    let root = repo(&cat(&[&shop(), &graph_only()]), &removed("src/strings.ts"));
    assert_eq!(runs(&decide(&opts(&root, clean_env(&[]), None))), ["test/strings.test.ts"]);
}

#[test]
fn always_runs_critical_tests_and_goes_full_on_critical_paths() {
    let critical = files(&[(".picky.yml", "mode: enforce\nmodel: none\ncritical: [\"test/strings.test.ts\", \"db/**\"]\n")]);
    let root = repo(&cat(&[&shop(), &critical]), &files(&[("src/math.ts", "// x\n")]));
    assert!(runs(&decide(&opts(&root, clean_env(&[]), None))).contains(&"test/strings.test.ts".to_string()));

    let root = repo(&cat(&[&shop(), &critical]), &files(&[("db/001.sql", "select 1;")]));
    let d = decide(&opts(&root, clean_env(&[]), None));
    assert!(d.full);
    assert_eq!(d.reason, "critical path db/001.sql changed");
}

#[test]
fn runs_everything_when_the_config_or_a_workflow_changes() {
    for (change, reason) in [
        (files(&[(".picky.yml", "mode: shadow\nmodel: none\n")]), ".picky.yml changed"),
        (files(&[(".github/workflows/ci.yml", "on: push")]), ".github/workflows/ci.yml changed"),
    ] {
        let d = decide(&opts(&repo(&cat(&[&shop(), &graph_only()]), &change), clean_env(&[]), None));
        assert!(d.full, "{reason}");
        assert_eq!(d.reason, reason);
    }
}

#[test]
fn runs_one_ecosystem_in_full_when_its_lockfile_changes() {
    let root = repo(&cat(&[&shop(), &graph_only()]), &files(&[("package-lock.json", "{}")]));
    let d = decide(&opts(&root, clean_env(&[]), None));
    assert!(!d.full);
    assert_eq!(d.ecosystems.as_ref().unwrap().get("js").map(String::as_str), Some("package-lock.json changed"));
    assert_eq!(runs(&d), ["test/cart.test.ts", "test/math.test.ts", "test/strings.test.ts"]);
    assert_eq!(d.tests["test/math.test.ts"].reason, "package-lock.json changed");
}

#[test]
fn runs_everything_on_non_pr_events_in_github_actions() {
    let root = repo(&cat(&[&shop(), &graph_only()]), &files(&[("src/math.ts", "// x\n")]));
    let d = decide(&opts(&root, clean_env(&[("GITHUB_ACTIONS", "true"), ("GITHUB_EVENT_NAME", "push")]), None));
    assert!(d.full);
    assert_eq!(d.reason, "event \"push\" always runs everything");
}

#[test]
fn runs_everything_without_an_api_key_or_with_config_it_cannot_honor() {
    let root = repo(&cat(&[&shop(), &jev()]), &files(&[("src/math.ts", "// x\n")]));
    let no_key = decide(&opts(&root, clean_env(&[]), None));
    assert!(no_key.full);
    assert_eq!(no_key.reason, "no API key (set PICKY_API_KEY)");

    let preset = files(&[(".picky.yml", "extends: github:org/.github/picky.yml@v1\nmodel: none\n")]);
    let root = repo(&cat(&[&shop(), &preset]), &files(&[("src/math.ts", "// x\n")]));
    let ext = decide(&opts(&root, clean_env(&[]), None));
    assert!(ext.full);
    assert_eq!(ext.reason, "`extends` is not supported in this version yet");
}

fn judge_change() -> Files {
    files(&[("src/strings.ts", "export const shout = (s: string) => s.toUpperCase() + \"!\";\n"), ("config/app.yml", "tax: 0.2\n")])
}

fn question_ids(body: &Value) -> Vec<String> {
    body["questions"].as_object().unwrap().keys().cloned().collect()
}

#[test]
fn asks_one_yes_no_question_per_test_outside_the_floor_and_keeps_those_at_or_above_the_threshold() {
    let root = repo(&cat(&[&shop(), &jev()]), &judge_change());
    let fetch = Mock::new(|_, body| {
        let ids = question_ids(body);
        jev_reply(&[(&ids[0], 0.9), (&ids[1], THRESHOLD / 2.0)])
    });
    let d = decide(&opts(&root, clean_env(&[("PICKY_API_KEY", "k")]), Some(fetch.clone())));

    assert_eq!(fetch.count(), 1);
    let calls = fetch.calls.lock().unwrap();
    let body = &calls[0].body;
    assert_eq!(body["model"], "jev-1.14.0");
    let state = body["state"].as_str().unwrap();
    assert!(state.contains("config/app.yml") && state.contains("tax: 0.2"));
    assert!(body["questions"].as_object().unwrap().values().all(|q| q["type"] == "noul"));
    assert_eq!(question_ids(body).len(), 2); // math and cart; strings is in the floor
    let auth = calls[0].headers.iter().find(|(k, _)| k == "authorization").map(|(_, v)| v.as_str());
    assert_eq!(auth, Some("Bearer k"));

    let strings = &d.tests["test/strings.test.ts"];
    assert!(strings.run);
    assert_eq!(strings.reason, "imports src/strings.ts");
    assert!(d.tests["test/cart.test.ts"].run);
    assert_eq!(d.tests["test/cart.test.ts"].p, Some(0.9));
    assert!(!d.tests["test/math.test.ts"].run);
    assert_eq!(d.tests["test/math.test.ts"].p, Some(THRESHOLD / 2.0));
    let usage = d.model_usage.unwrap();
    assert_eq!((usage.model.as_str(), usage.questions, usage.requests, usage.input_tokens), ("jev-1.14.0", 2, 1, 321.0));
}

#[test]
fn asks_about_a_rust_crate_by_its_unit_tests_and_a_kotlin_test_by_its_test_names() {
    let root = repo(&cat(&[&shop_rust(), &shop_kotlin(), &jev()]), &files(&[("config/app.yml", "tax: 0.2\n")]));
    let fetch = Mock::new(|_, _| jev_reply(&[]));
    decide(&opts(&root, clean_env(&[("PICKY_API_KEY", "k")]), Some(fetch.clone())));
    let calls = fetch.calls.lock().unwrap();
    let questions: Vec<String> = calls
        .iter()
        .flat_map(|c| c.body["questions"].as_object().unwrap().values().map(|q| q["instructions"].as_str().unwrap().to_string()))
        .collect();
    let about = |needle: &str| questions.iter().find(|q| q.contains(needle)).cloned().unwrap_or_default();
    assert!(about("crates/cart").contains("the unit tests and doctests of the Rust crate in \"crates/cart\""));
    assert!(about("crates/cart").contains("Test names: totals."));
    assert!(about("crates/money/tests/arith.rs").contains("Test names: adds."));
    assert!(about("core/src/test/kotlin/shop/CartTest.kt").contains("Test names: totals items."));
}

#[test]
fn records_no_model_usage_when_the_graph_decides_everything() {
    let root = repo(&cat(&[&shop(), &jev()]), &files(&[("README.md", "# docs\n")]));
    let fetch = Mock::new(|_, _| jev_reply(&[]));
    let d = decide(&opts(&root, clean_env(&[("PICKY_API_KEY", "k")]), Some(fetch.clone())));
    assert_eq!(fetch.count(), 0);
    assert!(d.model_usage.is_none());
}

#[test]
fn runs_the_candidates_when_the_model_fails_and_retries_rate_limits() {
    let root = repo(&cat(&[&shop(), &jev()]), &judge_change());
    let fetch = Mock::new(|i, _| match i {
        0 => Response { status: 429, body: Ok("slow down".into()) },
        _ => Response { status: 401, body: Ok("bad key".into()) },
    });
    let d = decide(&opts(&root, clean_env(&[("PICKY_API_KEY", "k")]), Some(fetch.clone())));
    assert_eq!(fetch.count(), 2);
    assert_eq!(runs(&d), ["test/cart.test.ts", "test/math.test.ts", "test/strings.test.ts"]);
    assert_eq!(d.tests["test/math.test.ts"].reason, "model unavailable (HTTP 401: bad key)");
    let usage = d.model_usage.unwrap();
    assert_eq!(usage.error.as_deref(), Some("HTTP 401: bad key"));
    assert_eq!(usage.questions, 2);
}

#[test]
fn runs_a_candidate_the_model_did_not_answer() {
    let root = repo(&cat(&[&shop(), &jev()]), &judge_change());
    let fetch = Mock::new(|_, _| jev_reply(&[]));
    let d = decide(&opts(&root, clean_env(&[("PICKY_API_KEY", "k")]), Some(fetch)));
    assert!(d.tests["test/math.test.ts"].run);
    assert_eq!(d.tests["test/math.test.ts"].reason, "model gave no answer");
}

#[test]
fn every_job_reuses_the_decision_one_job_computed_so_the_model_is_asked_once() {
    let root = repo(&cat(&[&shop(), &jev()]), &files(&[("src/math.ts", "// x\n")]));
    let runner_temp = temp_dir();
    let env = |job: &str| clean_env(&[("PICKY_API_KEY", "k"), ("RUNNER_TEMP", &runner_temp), ("GITHUB_JOB", job)]);
    let fetch = Mock::new(|_, _| jev_reply(&[("t0", 0.9)]));
    let f: Option<Arc<dyn Fetch>> = Some(fetch.clone());

    let first = decide_cached(&opts(&root, env("picky"), f.clone()));
    assert!(!first.cached && first.report);
    assert_eq!(first.decision.job.as_deref(), Some("picky"));
    let strings = &first.decision.tests["test/strings.test.ts"];
    assert!(strings.run);
    assert_eq!(strings.p, Some(0.9));

    // Another job with the artifact downloaded into its runner temp: same answers, no model call, reported once.
    let js = decide_cached(&opts(&root, env("js"), f.clone()));
    assert!(js.cached && js.report);
    assert_eq!(js.decision.tests, first.decision.tests);
    assert!(!decide_cached(&opts(&root, env("js"), f.clone())).report);
    assert!(!decide_cached(&opts(&root, env("picky"), f.clone())).report);
    assert_eq!(fetch.count(), 1);
}

#[test]
fn shares_the_decision_with_a_container_job_which_sees_the_same_event_at_another_path() {
    let root = repo(&cat(&[&shop(), &jev()]), &files(&[("src/math.ts", "// x\n")]));
    let runner_temp = temp_dir();
    let fetch = Mock::new(|_, _| jev_reply(&[("t0", 0.9)]));
    let f: Option<Arc<dyn Fetch>> = Some(fetch.clone());
    let run = |job: &str, event: &str| {
        let dir = temp_dir();
        write_files(&dir, &files(&[("event.json", event)]));
        let path = format!("{}/event.json", &*dir);
        let env = clean_env(&[
            ("PICKY_API_KEY", "k"),
            ("RUNNER_TEMP", &runner_temp),
            ("GITHUB_JOB", job),
            ("GITHUB_EVENT_NAME", "pull_request"),
            ("GITHUB_EVENT_PATH", &path),
        ]);
        decide_cached(&opts(&root, env, f.clone()))
    };
    run("picky", r#"{"pull_request":{"number":7}}"#);
    // The second job (Windows) gets the same event with a BOM, CRLF and indentation.
    let alpine = run("alpine", "\u{FEFF}{\r\n  \"pull_request\": {\r\n    \"number\": 7\r\n  }\r\n}\r\n");
    assert!(alpine.cached && alpine.report);
    assert_eq!(fetch.count(), 1);
}

#[test]
fn ignores_crlf_in_the_config_and_names_the_input_that_differs() {
    let root = repo(&cat(&[&shop(), &jev()]), &files(&[("src/math.ts", "// x\n")]));
    let runner_temp = temp_dir();
    let env = |job: &str, extra: &[(&str, &str)]| {
        let mut e = clean_env(&[("PICKY_API_KEY", "k"), ("RUNNER_TEMP", &runner_temp), ("GITHUB_JOB", job)]);
        e.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        e
    };
    let fetch = Mock::new(|_, _| jev_reply(&[("t0", 0.9)]));
    let f: Option<Arc<dyn Fetch>> = Some(fetch.clone());
    decide_cached(&opts(&root, env("picky", &[]), f.clone()));

    write_files(&root, &files(&[(".picky.yml", "mode: enforce\r\nmodel: jev\r\n")]));
    assert!(decide_cached(&opts(&root, env("windows", &[]), f.clone())).cached);

    let other = decide_cached(&opts(&root, env("shadow", &[("PICKY_MODE", "shadow")]), f.clone()));
    assert!(!other.cached);
    assert_eq!(other.mismatch.as_deref(), Some("the decision from job \"picky\" was made from a different config"));
}
