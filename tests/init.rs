mod common;

use common::*;
use picky::init::{Binaries, Content, Platform, SETUP_ACTION, edit_workflow, plan_init};
use picky::yaml::{Value, parse};

const WORKFLOW: &str = "name: CI
on:
  pull_request:
  push:
    branches: [main]

jobs:
  js:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - run: npx vitest run
      - run: npx tsc # types

  php:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: composer install
      - run: |
          echo \"unit tests\"
          vendor/bin/phpunit
";

fn project(extra: &Files) -> Dir {
    let root = temp_dir();
    write_files(&root, &cat(&[&shop(), &shop_php(), extra]));
    root
}

/// The workflow as JSON, the way `YAML.parse` reads it.
fn yaml(text: &str) -> serde_json::Value {
    fn json(v: &Value) -> serde_json::Value {
        match v {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => (*b).into(),
            Value::Number(n) => serde_json::json!(n),
            Value::String(s) => s.clone().into(),
            Value::Array(items) => items.iter().map(json).collect(),
            Value::Object(entries) => serde_json::Value::Object(entries.iter().map(|(k, v)| (k.clone(), json(v))).collect()),
        }
    }
    json(&parse(text).expect("valid YAML"))
}

fn keys(v: &serde_json::Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn marks_cargo_and_gradle_commands_not_maven_or_setup() {
    let root = temp_dir();
    write_files(&root, &cat(&[&files(&[(".git/HEAD", "ref: refs/heads/main\n")]), &shop_rust(), &shop_kotlin()]));
    let workflow = [
        "on: pull_request",
        "jobs:",
        "  build:",
        "    runs-on: ubuntu-latest",
        "    steps:",
        "      - uses: actions/checkout@v4",
        "      - run: cargo fetch",
        "      - run: cargo test --workspace --locked",
        "      - run: cargo clippy --all-targets -- -D warnings",
        "      - run: ./gradlew test --no-daemon",
        "      - run: mvn test",
        "",
    ]
    .join("\n");
    let text = edit_workflow(&workflow, &root).unwrap().text;
    for line in [
        "      - run: cargo test --workspace --locked # picky\n",
        "      - run: cargo clippy --all-targets -- -D warnings # picky\n",
        "      - run: ./gradlew test --no-daemon # picky\n",
        "      - run: cargo fetch\n",
        "      - run: mvn test\n",
    ] {
        assert!(text.contains(line), "{line}");
    }
}

#[test]
fn marks_recognized_commands_adds_the_env_lines_and_shares_one_decision_across_jobs() {
    let edit = edit_workflow(WORKFLOW, &project(&Files::new())).unwrap();
    let text = &edit.text;
    let wf = yaml(text);

    assert!(text.contains("      - run: npx vitest run # picky\n"));
    assert!(text.contains("          vendor/bin/phpunit # picky\n"));
    assert!(text.contains("          echo \"unit tests\"\n"));
    assert!(text.contains("      - run: npx tsc # types\n")); // an existing comment is left alone
    assert!(text.contains("      - run: npm ci\n")); // setup commands are skipped by the job-level rule instead
    assert_eq!(
        wf["env"],
        serde_json::json!({ "BASH_ENV": "${{ github.workspace }}/.github/picky/hook.sh", "PICKY_API_KEY": "${{ secrets.PICKY_API_KEY }}" })
    );
    assert_eq!(keys(&wf["jobs"]), ["picky", "js", "php"]);
    let steps = wf["jobs"]["picky"]["steps"].as_array().unwrap();
    let names: Vec<&str> = steps.iter().map(|s| s.get("uses").or(s.get("run")).unwrap().as_str().unwrap()).collect();
    assert_eq!(names, ["actions/checkout@v4", ".github/picky/picky decide", "actions/upload-artifact@v4"]);
    assert!(steps.iter().all(|s| s["continue-on-error"] == true));
    for job in ["js", "php"] {
        assert_eq!(wf["jobs"][job]["needs"], "picky");
        let first = &wf["jobs"][job]["steps"][0];
        assert_eq!(first["uses"], "actions/download-artifact@v4");
        assert_eq!(first["continue-on-error"], true);
        assert_eq!(first["with"], serde_json::json!({ "name": "picky-decision", "path": "${{ runner.temp }}/picky" }));
    }
    assert_eq!(edit.notes, ["marked 2 commands in 2 jobs"]);
    assert_eq!(edit.platforms, [Platform::LinuxX64]);
}

#[test]
fn is_idempotent_and_wires_in_a_job_marked_after_the_first_run() {
    let root = project(&Files::new());
    let once = edit_workflow(WORKFLOW, &root).unwrap().text;
    assert_eq!(edit_workflow(&once, &root).unwrap().text, once);

    let later = yaml(
        &edit_workflow(&format!("{once}\n  e2e:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npx vitest run\n"), &root)
            .unwrap()
            .text,
    );
    assert_eq!(later["jobs"]["e2e"]["needs"], "picky");
    assert_eq!(later["jobs"]["e2e"]["steps"][0]["uses"], "actions/download-artifact@v4");
    assert_eq!(keys(&later["jobs"]).iter().filter(|j| *j == "picky").count(), 1);
}

#[test]
fn keeps_existing_env_keys_and_needs_and_adds_no_decide_job_for_a_single_job() {
    let src = "on: [pull_request]
env:
  CI: true
  BASH_ENV: custom.sh
jobs:
  test:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - run: npx vitest run
";
    let out = yaml(&edit_workflow(src, &project(&Files::new())).unwrap().text);
    assert_eq!(out["env"], serde_json::json!({ "CI": true, "BASH_ENV": "custom.sh", "PICKY_API_KEY": "${{ secrets.PICKY_API_KEY }}" }));
    assert_eq!(keys(&out["jobs"]), ["test"]);
    assert_eq!(out["jobs"]["test"]["needs"], "build");
    assert_eq!(out["jobs"]["test"]["steps"][0]["run"], "npx vitest run");
}

#[test]
fn shares_the_decision_for_a_matrix_job_appending_picky_to_its_needs() {
    let src = "on: pull_request
jobs:
  test:
    needs: [lint, build]
    strategy:
      matrix:
        node: [20, 22]
    runs-on: ubuntu-latest
    steps:
      - run: npx vitest run
";
    let out = yaml(&edit_workflow(src, &project(&Files::new())).unwrap().text);
    assert_eq!(out["jobs"]["test"]["needs"], serde_json::json!(["lint", "build", "picky"]));
    assert_eq!(keys(&out["jobs"]), ["picky", "test"]);
}

#[test]
fn leaves_workflows_alone_when_picky_has_nothing_to_do_there() {
    let push = "on: push\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npx vitest run\n";
    let edit = edit_workflow(push, &project(&Files::new())).unwrap();
    assert_eq!(edit.text, push);
    assert_eq!(edit.notes, ["not changed: it does not run on pull_request"]);

    let unknown = "on: pull_request\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: make all\n";
    assert_eq!(edit_workflow(unknown, &project(&Files::new())).unwrap().text, unknown);
}

#[test]
fn runs_windows_steps_through_the_picky_shell_after_the_setup_action() {
    let root = project(&Files::new());
    let edit = edit_workflow(&WORKFLOW.replace("ubuntu-latest", "windows-latest"), &root).unwrap();
    let out = &edit.text;
    let wf = yaml(out);
    let steps: Vec<String> = wf["jobs"]["js"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| match s.get("uses") {
            Some(u) => u.as_str().unwrap().to_string(),
            None => format!("{} | {}", s["run"].as_str().unwrap().trim(), s.get("shell").and_then(|v| v.as_str()).unwrap_or("")),
        })
        .collect();
    assert_eq!(
        steps,
        [
            "actions/download-artifact@v4",
            "actions/checkout@v4",
            "./.github/picky",
            "npm ci | picky pwsh {0}",
            "npx vitest run | picky pwsh {0}",
            "npx tsc | picky pwsh {0}",
        ]
    );
    let php = wf["jobs"]["php"]["steps"].as_array().unwrap();
    assert_eq!(php.last().unwrap(), &serde_json::json!({ "run": "echo \"unit tests\"\nvendor/bin/phpunit\n", "shell": "picky pwsh {0}" }));
    assert!(!out.contains("# picky"));
    assert_eq!(edit_workflow(out, &root).unwrap().text, *out);
    // The decide job runs on Linux; the steps on Windows.
    assert_eq!(edit.platforms, [Platform::WindowsX64, Platform::LinuxX64]);
}

#[test]
fn lets_a_container_pick_its_own_shell_keeps_a_named_one_and_leaves_other_shells_alone() {
    let src = "on: pull_request
jobs:
  php:
    runs-on: ubuntu-latest
    container: composer:2
    steps:
      - uses: actions/checkout@v4
      - run: composer install
      - run: vendor/bin/phpunit
        shell: sh
      - run: npx vitest run
        shell: cmd
";
    let wf = yaml(&edit_workflow(src, &project(&Files::new())).unwrap().text);
    let shells: Vec<&str> = wf["jobs"]["php"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.get("shell").or(s.get("uses")).unwrap().as_str().unwrap())
        .collect();
    assert_eq!(shells, ["actions/checkout@v4", "./.github/picky", "picky {0}", "picky sh {0}", "cmd"]);

    // Two container jobs share one decision; inside a container the runner's temp dir is /__w/_temp.
    let two = src.replace("jobs:\n", "jobs:\n  js:\n    runs-on: ubuntu-latest\n    steps:\n      - run: npx vitest run\n");
    let shared = yaml(&edit_workflow(&two, &project(&Files::new())).unwrap().text);
    assert_eq!(shared["jobs"]["php"]["steps"][0]["with"]["path"], "/__w/_temp/picky");
    assert_eq!(shared["jobs"]["js"]["steps"][0]["with"]["path"], "${{ runner.temp }}/picky");
}

#[test]
fn skips_a_fallback_job_without_checkout_since_the_setup_action_lives_in_the_repo() {
    let src = "on: pull_request\njobs:\n  t:\n    runs-on: windows-latest\n    steps:\n      - run: npx vitest run\n";
    let r = edit_workflow(src, &project(&Files::new())).unwrap();
    assert_eq!(r.text, src);
    assert!(r.notes.contains(&"job \"t\" skipped: it needs actions/checkout before picky's setup step".to_string()));
}

#[test]
fn vendors_a_binary_per_runner_platform() {
    let src = |runs_on: &str| format!("on: pull_request\njobs:\n  t:\n    runs-on: {runs_on}\n    steps:\n      - run: npx vitest run\n");
    let root = project(&Files::new());
    let platforms = |runs_on: &str| edit_workflow(&src(runs_on), &root).unwrap().platforms;
    assert_eq!(platforms("ubuntu-24.04-arm"), [Platform::LinuxArm64]);
    assert_eq!(platforms("macos-latest"), [Platform::DarwinArm64]);
    assert_eq!(platforms("macos-13"), [Platform::DarwinX64]);
    assert_eq!(platforms("[self-hosted, linux, arm64]"), [Platform::LinuxArm64]);
    let matrix = "on: pull_request\njobs:\n  t:\n    strategy:\n      matrix:\n        os: [ubuntu-latest, macos-14]\n    runs-on: ${{ matrix.os }}\n    steps:\n      - run: npx vitest run\n";
    assert_eq!(edit_workflow(matrix, &root).unwrap().platforms, [Platform::LinuxX64, Platform::DarwinArm64]);
    // A runner picky can't read gets the binaries for the hosted runners, with a note.
    let unknown = edit_workflow(&src("${{ inputs.runner }}"), &root).unwrap();
    assert_eq!(unknown.platforms, [Platform::LinuxX64, Platform::WindowsX64, Platform::DarwinArm64]);
    assert!(unknown.notes.iter().any(|n| n.contains("can't tell its runner")), "{:?}", unknown.notes);
}

/// Binaries that are each platform's name, counting the calls.
struct Fake {
    calls: Vec<Platform>,
}

impl Binaries for Fake {
    fn get(&mut self, platform: Platform, current: Option<&[u8]>) -> Result<Option<Vec<u8>>, String> {
        self.calls.push(platform);
        let bytes = format!("binary {}", platform.as_str()).into_bytes();
        Ok(if current == Some(bytes.as_slice()) { None } else { Some(bytes) })
    }
}

fn apply(root: &str, plan: &picky::init::InitPlan) {
    for f in &plan.files {
        let full = format!("{root}/{}", f.path);
        match &f.after {
            None => std::fs::remove_file(&full).unwrap(),
            Some(after) => {
                std::fs::create_dir_all(std::path::Path::new(&full).parent().unwrap()).unwrap();
                std::fs::write(&full, after.bytes()).unwrap();
            }
        }
    }
}

#[test]
fn plan_init_lists_the_vendored_files_and_the_workflow_edit_and_nothing_once_applied() {
    let root = project(&files(&[(".github/workflows/ci.yml", WORKFLOW)]));
    let mut fake = Fake { calls: Vec::new() };
    let plan = plan_init(&root, &mut fake).unwrap();
    let paths: Vec<&str> = plan.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            ".picky.yml",
            ".github/picky/hook.sh",
            ".github/picky/picky",
            ".github/picky/picky.cmd",
            ".github/picky/action.yml",
            ".github/picky/bin/picky-linux-x64",
            ".github/workflows/ci.yml",
        ]
    );
    let mode = |p: &str| plan.files.iter().find(|f| f.path == p).unwrap().mode;
    assert_eq!(mode(".github/picky/picky"), Some(0o755));
    assert_eq!(mode(".github/picky/bin/picky-linux-x64"), Some(0o755));
    assert_eq!(plan.files[5].after, Some(Content::Binary(b"binary linux-x64".to_vec())));

    apply(&root, &plan);
    assert_eq!(plan_init(&root, &mut fake).unwrap().files, []);
}

#[test]
fn plan_init_keeps_vendored_binaries_up_to_date_and_removes_the_node_bundle() {
    let root = project(&files(&[
        (".github/workflows/ci.yml", WORKFLOW),
        (".github/picky/picky.mjs", "// the Node version\n"),
        (".github/picky/bin/picky-darwin-arm64", "old"),
    ]));
    let plan = plan_init(&root, &mut Fake { calls: Vec::new() }).unwrap();
    let changes: Vec<(&str, bool)> = plan.files.iter().map(|f| (f.path.as_str(), f.after.is_some())).collect();
    assert!(changes.contains(&(".github/picky/bin/picky-linux-x64", true)));
    assert!(changes.contains(&(".github/picky/bin/picky-darwin-arm64", true)));
    assert!(changes.contains(&(".github/picky/picky.mjs", false)));
}

#[test]
fn the_setup_action_is_a_valid_composite_action() {
    let action = yaml(SETUP_ACTION);
    assert!(action["description"].as_str().unwrap().contains("shell: picky {0}"));
    assert_eq!(action["runs"]["using"], "composite");
    assert_eq!(action["runs"]["steps"].as_array().unwrap().len(), 2);
}

/// The runner label for this machine, and the binary name the wrapper looks for.
#[cfg(unix)]
fn this_runner() -> (&'static str, &'static str) {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("macos-latest", "darwin-arm64"),
        ("macos", _) => ("macos-13", "darwin-x64"),
        (_, "aarch64") => ("ubuntu-24.04-arm", "linux-arm64"),
        _ => ("ubuntu-latest", "linux-x64"),
    }
}

#[cfg(unix)]
#[test]
fn picky_init_writes_a_setup_whose_wrapper_runs_the_vendored_binary() {
    use std::process::Command;
    let (runs_on, platform) = this_runner();
    let workflow = format!(
        "on: pull_request\njobs:\n  t:\n    runs-on: {runs_on}\n    steps:\n      - uses: actions/checkout@v4\n      - run: npx vitest run\n"
    );
    let root = repo(&cat(&[&shop(), &files(&[(".github/workflows/ci.yml", &workflow)])]), &Files::new());
    let dist = temp_dir();
    std::fs::copy(PICKY, format!("{}/picky-{platform}", &*dist)).unwrap();
    let init = |args: &[&str]| {
        let out = Command::new(PICKY).arg("init").args(args).current_dir(&*root).env_clear().envs(clean_env(&[])).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };

    let dry = init(&["--dist", &dist]);
    assert!(dry.contains(&format!("new file .github/picky/bin/picky-{platform} (")), "{dry}");
    assert!(dry.contains("picky: 7 files would change; run `picky init --write` to apply"), "{dry}");
    assert!(!std::path::Path::new(&format!("{}/.github/picky", &*root)).exists());

    let wrote = init(&["--write", "--dist", &dist]);
    assert!(wrote.contains("picky: wrote .picky.yml, .github/picky/hook.sh"), "{wrote}");
    let version = Command::new(format!("{}/.github/picky/picky", &*root)).arg("--version").output().unwrap();
    assert_eq!(String::from_utf8(version.stdout).unwrap(), format!("{}\n", env!("CARGO_PKG_VERSION")));
    // Like the TypeScript version, a second run still reports the marked job.
    assert_eq!(
        init(&["--dist", &dist]),
        "picky: .github/workflows/ci.yml: marked 0 commands in 1 job\npicky: already set up; nothing to change\n"
    );
}
