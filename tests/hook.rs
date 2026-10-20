//! The bash hook, run the way GitHub Actions runs a step.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use common::*;
use picky::hook::marked_lines;

fn math_change() -> Files {
    files(&[("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")])
}

#[test]
fn marks_commands_with_a_trailing_marker_or_every_command_after_a_standalone_marker() {
    assert_eq!(marked_lines("npm ci\nnpm test # picky\necho \"# pickyish\"", false), [2]);
    assert_eq!(marked_lines("npm ci\n# picky\nnpm test\n\n# note\nnpx vitest", false), [3, 6]);
}

#[test]
fn never_touches_heredocs_or_continued_lines() {
    let script = ["cat <<EOF", "vitest # picky", "EOF", "vitest \\", "  --run # picky", "jest # picky"].join("\n");
    assert_eq!(marked_lines(&script, false), [6]);
}

struct Run {
    code: Option<i32>,
    out: String,
    err: String,
}

/// Runs a step the way GitHub Actions does: bash --noprofile --norc -eo pipefail <script>, with BASH_ENV set.
fn run_step(root: &str, script: &str, env: &[(&str, &str)]) -> Run {
    let file = format!("{root}/.step.sh");
    std::fs::write(&file, script).unwrap();
    let hook = format!("{HOME}/hook/hook.sh");
    let mut vars = clean_env(&[("BASH_ENV", &hook), ("PICKY_BIN", PICKY)]);
    vars.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail", &file])
        .current_dir(root)
        .env_clear()
        .envs(&vars)
        .output()
        .unwrap();
    Run { code: out.status.code(), out: String::from_utf8_lossy(&out.stdout).into(), err: String::from_utf8_lossy(&out.stderr).into() }
}

fn gh(root: &str) -> Vec<(&'static str, String)> {
    vec![
        ("GITHUB_WORKFLOW_REF", "acme/shop/.github/workflows/ci.yml@refs/pull/7/merge".to_string()),
        ("GITHUB_JOB", "test".to_string()),
        ("GITHUB_WORKSPACE", root.to_string()),
    ]
}

fn pairs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, s)| (*k, s.as_str())).collect()
}

fn node_tests() -> Files {
    files(&[
        ("test/math.test.ts", "import { test } from 'node:test';\nimport '../src/math.ts';\ntest('math', () => {});\n"),
        ("test/cart.test.ts", "import { test } from 'node:test';\nimport '../src/cart.ts';\ntest('cart', () => {});\n"),
        ("test/strings.test.ts", "import { test } from 'node:test';\nimport '../src/strings.ts';\ntest('strings', () => {});\n"),
    ])
}

fn failing(name: &str) -> String {
    format!("import {{ test }} from 'node:test';\nimport '../src/{name}.ts';\ntest('{name}', () => {{ throw new Error('boom'); }});\n")
}

fn base() -> Files {
    cat(&[&shop(), &node_tests(), &graph_only()])
}

fn make_executable(path: &str) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn narrows_a_marked_test_command_and_leaves_unmarked_lines_alone() {
    let root = repo(&base(), &math_change());
    let r = run_step(&root, "echo before\nnode --test # picky\necho after\n", &[]);
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.out.contains("before") && r.out.contains("after"));
    assert!(r.out.contains("✔ math") && r.out.contains("✔ cart"));
    assert!(!r.out.contains("✔ strings"));
    assert!(r.err.contains("picky: $ node --test test/cart.test.ts test/math.test.ts"), "{}", r.err);
}

#[test]
fn keeps_e_semantics_a_failing_selected_test_fails_the_step() {
    let root = repo(&base(), &cat(&[&files(&[("test/math.test.ts", &failing("math"))]), &math_change()]));
    let r = run_step(&root, "node --test # picky\necho unreachable\n", &[]);
    assert_ne!(r.code, Some(0));
    assert!(!r.out.contains("unreachable"));
}

#[test]
fn runs_the_original_command_when_picky_itself_fails() {
    let root = repo(&base(), &math_change());
    // A picky that prepares the step fine but crashes when asked about a line.
    let broken = format!("{}/broken-picky", &*root);
    std::fs::write(&broken, format!("#!/bin/sh\n[ \"$1\" = hook-exec ] && exec \"{PICKY}\" \"$@\"\nexit 3\n")).unwrap();
    make_executable(&broken);
    let r = run_step(&root, "node --test # picky\n", &[("PICKY_BIN", &broken)]);
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.out.contains("✔ math"));
    assert!(r.out.contains("✔ strings")); // the unchanged command ran every test
}

#[test]
fn honors_a_marker_written_as_a_yaml_comment_on_a_one_line_run_step() {
    let wf = files(&[(
        ".github/workflows/ci.yml",
        "on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: node --test   # picky\n",
    )]);
    let root = repo(&cat(&[&base(), &wf]), &math_change());
    // GitHub hands the step script over without the comment: YAML already dropped it.
    let r = run_step(&root, "node --test\n", &pairs(&gh(&root)));
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.err.contains("picky: $ node --test test/cart.test.ts test/math.test.ts"), "{}", r.err);
}

#[test]
fn runs_unmarked_steps_of_a_marked_workflow_as_they_are_keeping_dollar_0() {
    let wf = files(&[(
        ".github/workflows/ci.yml",
        "on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo \"$0\"\n      - run: node --test # picky\n",
    )]);
    let root = repo(&cat(&[&base(), &wf]), &math_change());
    let r = run_step(&root, "echo \"$0\"\n", &pairs(&gh(&root)));
    assert_eq!(r.code, Some(0));
    assert_eq!(r.out.trim(), format!("{}/.step.sh", &*root));
}

#[test]
fn skips_the_job_setup_only_when_every_marked_command_in_the_job_skips() {
    let wf = files(&[
        (
            ".github/workflows/ci.yml",
            "on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: npm ci\n      - run: node --test # picky\n",
        ),
        ("fake-bin/npm", "#!/bin/sh\necho \"npm-ran $*\"\n"),
    ]);
    let fake = |root: &str| {
        let mut env = gh(root);
        env.push(("PATH", format!("{root}/fake-bin:{}", std::env::var("PATH").unwrap())));
        env
    };

    let docs = repo(&cat(&[&base(), &wf]), &files(&[("README.md", "# docs\n")]));
    make_executable(&format!("{}/fake-bin/npm", &*docs));
    let skipped = run_step(&docs, "npm ci\n", &pairs(&fake(&docs)));
    assert_eq!(skipped.code, Some(0));
    assert!(!skipped.out.contains("npm-ran"));
    assert!(skipped.err.contains("picky: npm ci -> skip: all 1 marked commands in this job skip"), "{}", skipped.err);

    let code = repo(&cat(&[&base(), &wf]), &math_change());
    make_executable(&format!("{}/fake-bin/npm", &*code));
    let ran = run_step(&code, "npm ci\n", &pairs(&fake(&code)));
    assert!(ran.out.contains("npm-ran ci"));
}

/// The platform name the vendored wrapper looks for on this machine.
fn this_platform() -> String {
    let os = if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let cpu = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    format!("{os}-{cpu}")
}

#[test]
fn finds_the_picky_that_init_vendored_next_to_the_hook_when_picky_bin_is_unset() {
    let root = repo(&base(), &math_change());
    let dir = format!("{}/.github/picky", &*root);
    std::fs::create_dir_all(format!("{dir}/bin")).unwrap();
    std::fs::write(format!("{dir}/hook.sh"), picky::init::HOOK).unwrap();
    std::fs::write(format!("{dir}/picky"), picky::init::WRAPPER).unwrap();
    make_executable(&format!("{dir}/picky"));
    let bin = format!("{dir}/bin/picky-{}", this_platform());
    std::fs::copy(PICKY, &bin).unwrap();
    make_executable(&bin);
    let r = run_step(&root, "node --test # picky\n", &[("BASH_ENV", &format!("{dir}/hook.sh")), ("PICKY_BIN", "")]);
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.err.contains("picky: $ node --test test/cart.test.ts test/math.test.ts"), "{}", r.err);
}

#[test]
fn runs_the_step_unchanged_when_no_vendored_binary_fits_the_runner() {
    let root = repo(&base(), &math_change());
    let dir = format!("{}/.github/picky", &*root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/hook.sh"), picky::init::HOOK).unwrap();
    std::fs::write(format!("{dir}/picky"), picky::init::WRAPPER).unwrap();
    make_executable(&format!("{dir}/picky"));
    // No binary: PATH without a `picky` either, so the hook leaves the step alone.
    let r = run_step(&root, "node --test # picky\n", &[("BASH_ENV", &format!("{dir}/hook.sh")), ("PICKY_BIN", "")]);
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert!(r.out.contains("✔ strings")); // every test ran
    assert!(r.err.contains("picky: binary not found; running this step unchanged"), "{}", r.err);

    // `shell: picky {0}` through the wrapper: the step still runs, in sh -e.
    let step = format!("{}/.step", &*root);
    std::fs::write(&step, "echo ran\nfalse\necho unreachable\n").unwrap();
    let out = Command::new(format!("{dir}/picky")).arg(&step).current_dir(&*root).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains(&format!("picky: no picky binary for {} in {dir}/bin; running the step unchanged", this_platform()))
    );
    let out = Command::new(format!("{dir}/picky")).args(["bash", &step]).current_dir(&*root).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
}

fn shadow() -> Files {
    files(&[(".picky.yml", "mode: shadow\nmodel: none\n")])
}

fn actions(root: &str) -> Vec<(&'static str, String)> {
    vec![("GITHUB_ACTIONS", "true".to_string()), ("GITHUB_STEP_SUMMARY", format!("{root}/.summary.md"))]
}

#[test]
fn shadow_reports_a_failure_in_a_test_enforce_mode_would_have_skipped() {
    let root = repo(&cat(&[&shop(), &node_tests(), &shadow(), &files(&[("test/strings.test.ts", &failing("strings"))])]), &math_change());
    let r = run_step(&root, "node --test # picky\necho unreachable\n", &pairs(&actions(&root)));
    assert_ne!(r.code, Some(0));
    assert!(!r.out.contains("unreachable"));
    assert!(r.err.contains("picky: node --test -> shadow mode: would run 2 of 3 test files; running in full"), "{}", r.err);
    assert!(r.err.contains("picky: $ node --test test/cart.test.ts test/math.test.ts"));
    let miss = "enforce mode would have run only `node --test test/cart.test.ts test/math.test.ts` (running 2 of 3 test files), which passes, while the full `node --test` failed";
    assert!(r.err.contains(&format!("picky: SHADOW MISS: {miss}.")), "{}", r.err);
    assert!(r.out.contains(&format!("::warning title=picky shadow miss::{miss}\n")));
    assert!(r.out.contains("::group::picky shadow check\n"));
    let summary = std::fs::read_to_string(format!("{}/.summary.md", &*root)).unwrap();
    assert!(summary.contains(&format!("### picky: shadow miss\n{miss}.\n")), "{summary}");
}

#[test]
fn shadow_stays_quiet_when_enforce_mode_would_have_caught_the_failure_too() {
    let root = repo(&cat(&[&shop(), &node_tests(), &shadow(), &files(&[("test/math.test.ts", &failing("math"))])]), &math_change());
    let r = run_step(&root, "node --test # picky\n", &pairs(&actions(&root)));
    assert_ne!(r.code, Some(0));
    assert!(r.err.contains("picky: shadow check: that fails too, so enforce mode would have caught this failure"), "{}", r.err);
    assert!(!r.err.contains("SHADOW MISS"));
    assert!(!r.out.contains("::warning"));
}

#[test]
fn shadow_checks_the_prefix_form_too() {
    let root = repo(&cat(&[&shop(), &node_tests(), &shadow(), &files(&[("test/strings.test.ts", &failing("strings"))])]), &math_change());
    let out = Command::new(PICKY).args(["--", "node", "--test"]).current_dir(&*root).env_clear().envs(clean_env(&[])).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0));
    assert!(err.contains("picky: $ node --test test/cart.test.ts test/math.test.ts"), "{err}");
    assert!(err.contains("picky: SHADOW MISS: enforce mode would have run only `node --test test/cart.test.ts test/math.test.ts`"));
}

#[test]
fn shadow_runs_a_passing_step_once_with_no_check() {
    let root = repo(&cat(&[&shop(), &node_tests(), &shadow()]), &math_change());
    let r = run_step(&root, "node --test # picky\n", &[]);
    assert_eq!(r.code, Some(0), "{}", r.err);
    assert_eq!(r.out.matches("✔ math").count(), 1);
    assert!(r.out.contains("✔ strings"));
    assert!(!r.err.contains("shadow check"));
}

#[test]
fn runs_unmarked_steps_without_picky() {
    let root = repo(&base(), &math_change());
    let r = run_step(&root, "node --test\n", &[]);
    assert_eq!(r.code, Some(0));
    assert!(!r.err.contains("picky"));
    assert!(r.out.contains("✔ strings"));
}
