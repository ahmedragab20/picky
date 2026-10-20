mod common;

use common::*;
use picky::decide::{DecideOptions, decide};
use picky::job::{JobPlan, is_setup_command, plan_setup};
use picky::sh::tokenize;

const GH: &[(&str, &str)] = &[("GITHUB_WORKFLOW_REF", "acme/shop/.github/workflows/ci.yml@refs/pull/7/merge"), ("GITHUB_JOB", "test")];

fn workflow(steps: &str) -> Files {
    files(&[(".github/workflows/ci.yml", &format!("on: pull_request\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n{steps}"))])
}

const STEPS: &str =
    "      - uses: actions/checkout@v4\n      - uses: actions/setup-node@v4\n      - run: npm ci\n      - run: node --test # picky\n";

fn math_change() -> Files {
    files(&[("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")])
}

fn docs_change() -> Files {
    files(&[("README.md", "x")])
}

fn plan_in(root: &str, script: &str, env: &[(&str, &str)]) -> JobPlan {
    let opts = DecideOptions { root: root.to_string(), env: clean_env(env), fetch: None };
    plan_setup(script, root, &clean_env(env), || Ok(decide(&opts))).unwrap()
}

fn setup_plan(extra: &Files, change: &Files, script: &str, env: &[(&str, &str)]) -> JobPlan {
    let root = repo(&cat(&[&shop(), &graph_only(), extra]), change);
    plan_in(&root, script, env)
}

fn skip_all() -> JobPlan {
    JobPlan { skip: true, note: "skip: all 1 marked commands in this job skip".into() }
}

#[test]
fn recognizes_installs_not_commands_that_add_packages_or_run_scripts() {
    for cmd in [
        "npm ci",
        "npm install --no-audit",
        "pnpm i --frozen-lockfile",
        "yarn",
        "yarn --immutable",
        "bun install",
        "composer install -n --prefer-dist",
        "npx playwright install --with-deps chromium",
        "cargo fetch --locked",
        "cargo install cargo-nextest --locked",
        "cargo binstall -y cargo-nextest",
        "rustup component add clippy rustfmt",
        "rustup toolchain install stable --profile minimal",
    ] {
        assert!(is_setup_command(tokenize(cmd).as_deref()), "{cmd}");
    }
    for cmd in [
        "npm install lodash",
        "npm test",
        "yarn test",
        "composer require x/y",
        "composer update",
        "cargo build",
        "rustup run stable cargo test",
    ] {
        assert!(!is_setup_command(tokenize(cmd).as_deref()), "{cmd}");
    }
}

#[test]
fn skips_setup_when_every_marked_command_in_the_job_skips() {
    assert_eq!(setup_plan(&workflow(STEPS), &files(&[("README.md", "# docs\n")]), "npm ci", GH), skip_all());
}

#[test]
fn reads_markers_inside_block_scripts_too() {
    let block = workflow(
        &STEPS.replace("      - run: node --test # picky", "      - run: |\n          echo testing # picky\n          node --test # picky"),
    );
    let plan = setup_plan(&block, &docs_change(), "npm ci", GH);
    assert!(!plan.skip);
    assert!(plan.note.contains("`echo testing # picky`: command not recognized"), "{}", plan.note);
}

#[test]
fn keeps_setup_when_a_marked_command_runs() {
    let plan = setup_plan(&workflow(STEPS), &math_change(), "npm ci", GH);
    assert!(!plan.skip);
    assert!(plan.note.contains("`node --test`: running 2 of 3 test files"), "{}", plan.note);
}

#[test]
fn counts_picky_shell_steps_as_marked_e_g_on_windows() {
    let steps = "      - uses: actions/checkout@v4\n      - uses: ./.github/picky\n      - run: npm ci\n        shell: picky pwsh {0}\n      - run: node --test\n        shell: picky pwsh {0}\n";
    let windows = workflow(steps)[".github/workflows/ci.yml"].clone().unwrap().replace("ubuntu-latest", "windows-latest");
    assert_eq!(setup_plan(&files(&[(".github/workflows/ci.yml", &windows)]), &docs_change(), "npm ci", GH), skip_all());

    // Without picky as its shell, a Windows step is invisible to picky, so setup stays.
    let plain = windows.replace("      - run: node --test\n        shell: picky pwsh {0}\n", "      - run: node --test # picky\n");
    assert_eq!(
        setup_plan(&files(&[(".github/workflows/ci.yml", &plain)]), &docs_change(), "npm ci", GH),
        JobPlan { skip: false, note: "setup runs (picky can't decide `node --test` here)".into() }
    );
}

#[test]
fn skips_rust_toolchain_setup_when_the_marked_cargo_command_skips() {
    let steps = "      - uses: actions/checkout@v4\n      - uses: dtolnay/rust-toolchain@stable\n      - uses: Swatinem/rust-cache@v2\n      - uses: taiki-e/install-action@nextest\n      - run: cargo fetch\n      - run: cargo nextest run --workspace # picky\n";
    let root = repo(&cat(&[&shop_rust(), &graph_only(), &workflow(steps)]), &files(&[("README.md", "# docs\n")]));
    assert_eq!(plan_in(&root, "cargo fetch", GH), skip_all());
}

#[test]
fn keeps_setup_when_the_job_does_other_work() {
    let extra = workflow(&format!("{STEPS}      - run: npm run build\n"));
    assert_eq!(
        setup_plan(&extra, &docs_change(), "npm ci", GH),
        JobPlan { skip: false, note: "setup runs (the job also runs `npm run build`)".into() }
    );
    let action = workflow(&format!("{STEPS}      - uses: ./.github/actions/e2e\n"));
    assert_eq!(
        setup_plan(&action, &docs_change(), "npm ci", GH),
        JobPlan { skip: false, note: "setup runs (step uses ./.github/actions/e2e)".into() }
    );
}

#[test]
fn keeps_setup_when_it_cannot_match_the_step_to_the_workflow() {
    assert!(!setup_plan(&workflow(STEPS), &docs_change(), "npm ci --legacy-peer-deps", GH).skip);
    let lint = [GH[0], ("GITHUB_JOB", "lint")];
    assert!(!setup_plan(&workflow(STEPS), &docs_change(), "npm ci", &lint).skip);
    assert!(!setup_plan(&workflow(STEPS), &docs_change(), "npm ci", &[]).skip);
}

#[test]
fn keeps_setup_for_marked_steps_the_hook_does_not_run() {
    let pwsh = workflow(&STEPS.replace("      - run: node --test # picky", "      - run: node --test # picky\n        shell: pwsh"));
    assert!(!setup_plan(&pwsh, &docs_change(), "npm ci", GH).skip);
}

#[test]
fn only_reports_the_skip_in_shadow_mode() {
    let shadow = cat(&[&workflow(STEPS), &files(&[(".picky.yml", "mode: shadow\nmodel: none\n")])]);
    assert_eq!(
        setup_plan(&shadow, &docs_change(), "npm ci", GH),
        JobPlan { skip: false, note: "shadow mode: would skip setup (all 1 marked commands skip); running".into() }
    );
}
