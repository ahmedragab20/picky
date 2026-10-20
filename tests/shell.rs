mod common;

use std::cell::RefCell;
use std::collections::HashMap;

use common::*;
use picky::job::JobPlan;
use picky::plan::{Enforce, Outcome};
use picky::shell::{Shell, StepPlanner, finalize, quote_for, rewrite_step};

struct Planner {
    outcomes: HashMap<String, Outcome>,
}

impl StepPlanner for Planner {
    fn line(&mut self, words: &[String]) -> Outcome {
        self.outcomes
            .get(&words.join(" "))
            .cloned()
            .unwrap_or(Outcome::Full { note: "command not recognized; runs in full".into(), enforce: None })
    }
    fn setup(&mut self) -> Result<JobPlan, String> {
        Ok(JobPlan { skip: true, note: "skip: all 1 marked commands in this job skip".into() })
    }
    fn self_command(&self) -> Vec<String> {
        strings(&["/opt/node", "C:\\picky\\picky.mjs"])
    }
}

fn planner(outcomes: Vec<(&str, Outcome)>) -> Planner {
    Planner { outcomes: outcomes.into_iter().map(|(k, v)| (k.to_string(), v)).collect() }
}

#[test]
fn decides_every_command_of_the_step_in_the_shell_it_runs_in() {
    let script = "npm ci\r\nWrite-Host \"go\"\r\nnpx vitest run\r\nnpx tsc\r\nnpx eslint `\r\n  .\r\n";
    let logs = RefCell::new(Vec::new());
    let mut p = planner(vec![
        (
            "npx vitest run",
            Outcome::Narrow { argv: strings(&["npx", "vitest", "run", "test/a b.test.ts"]), note: "running 1 of 3 test files".into() },
        ),
        ("npx tsc", Outcome::Skip { note: "skip: no changes tsc checks".into() }),
    ]);
    let out = rewrite_step(script, Shell::Pwsh, &mut p, &|m| logs.borrow_mut().push(m.to_string())).unwrap();
    assert_eq!(
        out.split('\n').collect::<Vec<_>>(),
        ["# skipped by picky", "Write-Host \"go\"", "npx vitest run 'test/a b.test.ts'", "# skipped by picky", "npx eslint `", "  .", ""]
    );
    assert_eq!(
        logs.into_inner(),
        [
            "picky: npm ci -> skip: all 1 marked commands in this job skip",
            "picky: npx vitest run -> running 1 of 3 test files",
            "picky: $ npx vitest run 'test/a b.test.ts'",
            "picky: npx tsc -> skip: no changes tsc checks",
        ]
    );
}

#[test]
fn hands_shadow_mode_lines_to_picky_which_runs_them_in_full_and_checks_a_failure() {
    let shadow = Outcome::Full {
        note: "shadow mode: would skip: no changes tsc checks; running in full".into(),
        enforce: Some(Enforce::Skip { note: "skip: no changes tsc checks".into() }),
    };
    let picky = "/opt/node 'C:\\picky\\picky.mjs' shadow-report";
    let miss = "'enforce mode would have skipped `npx tsc` (no changes tsc checks), and it failed'";
    assert_eq!(
        rewrite_step("  npx tsc # picky\n", Shell::Pwsh, &mut planner(vec![("npx tsc", shadow.clone())]), &|_| {}).unwrap(),
        format!(
            "  npx tsc; if ($LASTEXITCODE) {{ $__picky_rc = $LASTEXITCODE; & {picky} skipped {miss}; $global:LASTEXITCODE = $__picky_rc }}\n"
        )
    );
    assert_eq!(
        rewrite_step("npx tsc\n", Shell::Bash, &mut planner(vec![("npx tsc", shadow)]), &|_| {}).unwrap(),
        format!("npx tsc || {{ __picky_rc=$?; {picky} skipped {miss} || true; (exit $__picky_rc); }}\n")
    );
}

#[test]
fn wraps_powershell_the_way_github_does_and_quotes_for_it() {
    assert_eq!(
        finalize("npx vitest", Shell::Pwsh),
        "$ErrorActionPreference = 'stop'\nnpx vitest\nif ((Test-Path -LiteralPath variable:\\LASTEXITCODE)) { exit $LASTEXITCODE }\n"
    );
    assert_eq!(finalize("npx vitest", Shell::Sh), "npx vitest");
    assert_eq!(quote_for(Shell::Pwsh, &["a", "it's", "$x"]), "a 'it''s' '$x'");
}

#[cfg(unix)]
fn node_tests() -> Files {
    files(&[
        ("test/math.test.ts", "import { test } from 'node:test';\nimport '../src/math.ts';\ntest('math', () => {});\n"),
        ("test/cart.test.ts", "import { test } from 'node:test';\nimport '../src/cart.ts';\ntest('cart', () => {});\n"),
        ("test/strings.test.ts", "import { test } from 'node:test';\nimport '../src/strings.ts';\ntest('strings', () => {});\n"),
    ])
}

/// `shell: picky sh {0}`.
#[cfg(unix)]
fn step(root: &str, script: &str) -> (Option<i32>, String, String) {
    let file = format!("{root}/.step");
    std::fs::write(&file, script).unwrap();
    let out = std::process::Command::new(PICKY).args(["sh", &file]).current_dir(root).env_clear().envs(clean_env(&[])).output().unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stdout).into(), String::from_utf8_lossy(&out.stderr).into())
}

#[cfg(unix)]
#[test]
fn narrows_the_tests_and_keeps_the_step_failing_when_a_command_fails() {
    let root = repo(
        &cat(&[&shop(), &node_tests(), &graph_only()]),
        &files(&[("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")]),
    );
    let (code, out, err) = step(&root, "echo before\nnode --test\n");
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("before"));
    assert!(out.contains("✔ cart"));
    assert!(!out.contains("✔ strings"));
    assert!(err.contains("picky: $ node --test test/cart.test.ts test/math.test.ts"), "{err}");

    let (code, out, _) = step(&root, "false\necho after\n");
    assert_eq!(code, Some(1));
    assert!(!out.contains("after"));
}

#[cfg(unix)]
#[test]
fn in_shadow_mode_reports_a_failing_command_that_enforce_mode_would_have_skipped() {
    let broken = files(&[(
        "test/strings.test.ts",
        "import { test } from 'node:test';\nimport '../src/strings.ts';\ntest('strings', () => { throw new Error('boom'); });\n",
    )]);
    let root = repo(
        &cat(&[&shop(), &node_tests(), &broken, &files(&[(".picky.yml", "mode: shadow\nmodel: none\n")])]),
        &files(&[("README.md", "# docs\n")]),
    );
    let (code, out, err) = step(&root, "node --test\necho after\n");
    assert_ne!(code, Some(0)); // shadow mode never changes the result
    assert!(out.contains("✖ strings"));
    assert!(err.contains("picky: node --test -> shadow mode: would skip: no affected tests (0 of 3); running in full"), "{err}");
    assert!(err.contains("picky: SHADOW MISS: enforce mode would have skipped `node --test` (no affected tests (0 of 3)), and it failed."));
}
