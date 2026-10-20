mod common;

use common::*;
use picky::decide::{DecideOptions, Decision, decide};
use picky::plan::{Enforce, Outcome, plan_command};

/// A mixed repo: TypeScript (shop) and PHP (shop_php), graph-only and enforcing.
fn mixed() -> Files {
    cat(&[&shop(), &shop_php(), &files(&[("eslint.config.js", "export default [];"), (".picky.yml", "mode: enforce\nmodel: none\n")])])
}

/// A Cargo workspace next to a Gradle build, graph-only and enforcing.
fn jvm_rust() -> Files {
    cat(&[&shop_rust(), &shop_kotlin(), &graph_only()])
}

fn edit(file: &str) -> Files {
    let text = jvm_rust()[file].clone().unwrap();
    files(&[(file, &format!("{text}\n// edited\n"))])
}

struct Plans {
    root: Dir,
    decision: Decision,
}

impl Plans {
    fn new(change: &Files, base: &Files) -> Plans {
        let root = repo(base, change);
        let decision = decide(&DecideOptions { root: root.to_string(), env: clean_env(&[]), fetch: None });
        Plans { root, decision }
    }
    fn plan(&self, cmd: &str) -> Outcome {
        let argv: Vec<String> = cmd.split(' ').map(str::to_string).collect();
        plan_command(&argv, &self.root, &self.root, &self.decision).unwrap()
    }
    fn narrow(&self, cmd: &str) -> Vec<String> {
        match self.plan(cmd) {
            Outcome::Narrow { argv, .. } => argv,
            other => panic!("{cmd}: expected narrow, got {other:?}"),
        }
    }
    fn is(&self, cmd: &str, action: &str) {
        let got = match self.plan(cmd) {
            Outcome::Full { .. } => "full",
            Outcome::Skip { .. } => "skip",
            Outcome::Narrow { .. } => "narrow",
        };
        assert_eq!(got, action, "{cmd}");
    }
}

fn full(note: &str) -> Outcome {
    Outcome::Full { note: note.into(), enforce: None }
}

#[test]
fn narrows_phpunit_to_the_tests_reaching_a_changed_class_and_skips_the_js_side() {
    let p = Plans::new(&files(&[("app/Money.php", "<?php\nnamespace App;\n\nfinal class Money { public int $cents = 0; }\n")]), &mixed());
    assert_eq!(p.narrow("vendor/bin/phpunit"), ["vendor/bin/phpunit", "tests/Unit/CartTest.php"]);
    p.is("node --test", "skip");
    p.is("npx tsc --noEmit", "skip");
    p.is("vendor/bin/phpstan analyse", "full");
    p.is("npx eslint .", "skip"); // ESLint doesn't check PHP files
}

#[test]
fn selects_the_php_tests_that_used_a_deleted_class() {
    let p = Plans::new(&removed("app/Models/User.php"), &mixed());
    assert_eq!(p.narrow("vendor/bin/pest"), ["vendor/bin/pest", "tests/Unit/UserTest.php"]);
}

#[test]
fn runs_one_ecosystem_in_full_when_its_lockfile_changes_and_keeps_narrowing_the_other() {
    let p =
        Plans::new(&files(&[("composer.lock", "{}"), ("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")]), &mixed());
    assert!(!p.decision.full);
    assert_eq!(p.plan("vendor/bin/phpunit"), full("full run for php commands: composer.lock changed"));
    assert_eq!(p.narrow("node --test"), ["node", "--test", "test/cart.test.ts", "test/math.test.ts"]);
}

#[test]
fn lints_changed_files_plus_their_dependents_and_type_checks_in_full() {
    let p = Plans::new(&files(&[("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")]), &mixed());
    assert_eq!(
        p.narrow("npx eslint ."),
        ["npx", "eslint", "--no-warn-ignored", "src/cart.ts", "src/math.ts", "test/cart.test.ts", "test/math.test.ts"]
    );
    assert_eq!(p.narrow("npx prettier --check ."), ["npx", "prettier", "--check", "--ignore-unknown", "src/math.ts"]);
    p.is("npx tsc --noEmit", "full");
    p.is("vendor/bin/phpstan analyse", "skip");
    p.is("vendor/bin/pint --test", "skip");
}

#[test]
fn formats_docs_only_changes_and_skips_everything_else() {
    let p = Plans::new(&files(&[("README.md", "# shop, documented\n")]), &mixed());
    assert_eq!(p.narrow("npx prettier --check ."), ["npx", "prettier", "--check", "--ignore-unknown", "README.md"]);
    // ESLint gets the file too: markdown plugins lint it, and --no-warn-ignored skips it otherwise.
    assert_eq!(p.narrow("npx eslint ."), ["npx", "eslint", "--no-warn-ignored", "README.md"]);
    for cmd in ["npx tsc", "vendor/bin/phpstan", "vendor/bin/phpunit", "node --test"] {
        p.is(cmd, "skip");
    }
}

#[test]
fn lints_everything_when_the_linter_config_or_a_file_it_imports_changes() {
    let with_rule = cat(&[
        &mixed(),
        &files(&[("eslint.config.js", "import './rules/local.js';\nexport default [];"), ("rules/local.js", "export {};")]),
    ]);
    let p = Plans::new(&files(&[("rules/local.js", "export const x = 1;")]), &with_rule);
    assert_eq!(p.plan("npx eslint ."), full("eslint config eslint.config.js affected; runs in full"));
}

#[test]
fn only_lints_files_under_the_command_targets() {
    let p = Plans::new(&files(&[("src/strings.ts", "export const shout = (s: string) => s + \"!\";\n")]), &mixed());
    assert_eq!(p.narrow("npx eslint src"), ["npx", "eslint", "--no-warn-ignored", "src/strings.ts"]);
    p.is("npx eslint lib", "skip");
}

#[test]
fn turns_every_skip_or_narrow_into_a_full_run_in_shadow_mode() {
    let p = Plans::new(&files(&[("README.md", "x")]), &cat(&[&mixed(), &files(&[(".picky.yml", "mode: shadow\nmodel: none\n")])]));
    assert_eq!(
        p.plan("npx tsc"),
        Outcome::Full {
            note: "shadow mode: would skip: no changes tsc checks; running in full".into(),
            enforce: Some(Enforce::Skip { note: "skip: no changes tsc checks".into() }),
        }
    );
}

#[test]
fn rust_runs_the_crates_a_change_reaches_and_skips_gradle() {
    let p = Plans::new(&edit("crates/money/src/fmt.rs"), &jvm_rust());
    assert_eq!(p.narrow("cargo test --workspace"), ["cargo", "test", "-p", "cart", "-p", "money"]);
    p.is("cargo clippy --all-targets", "full");
    p.is("./gradlew test", "skip");
    p.is("./gradlew check", "skip");
}

#[test]
fn rust_runs_only_the_integration_targets_a_test_helper_reaches() {
    let p = Plans::new(&edit("crates/money/tests/common/mod.rs"), &jvm_rust());
    assert_eq!(
        p.narrow("cargo nextest run"),
        ["cargo", "nextest", "run", "--no-tests=pass", "-p", "money", "--test", "arith", "--test", "format"]
    );
}

#[test]
fn rust_tests_a_crate_whose_readme_is_its_doctest_though_docs_are_low_risk() {
    let p = Plans::new(&files(&[("crates/money/README.md", "# money\n\n```\nassert_eq!(money::add(2, 2), 4);\n```\n")]), &jvm_rust());
    assert_eq!(p.narrow("cargo test"), ["cargo", "test", "-p", "cart", "-p", "money"]);
    match p.plan("cargo clippy") {
        Outcome::Full { note, .. } => assert!(note.contains("read a changed file"), "{note}"),
        other => panic!("{other:?}"),
    }
    p.is("./gradlew test", "skip");
}

#[test]
fn rust_tests_the_crate_of_a_removed_module() {
    let base = cat(&[&jvm_rust(), &files(&[("crates/strings/src/extra.rs", "pub fn x() {}\n")])]);
    let p = Plans::new(&removed("crates/strings/src/extra.rs"), &base);
    assert_eq!(p.narrow("cargo test"), ["cargo", "test", "-p", "strings"]);
}

#[test]
fn rust_runs_every_command_in_full_when_cargo_lock_changes() {
    let p = Plans::new(&files(&[("Cargo.lock", "version = 4\n# bumped\n")]), &jvm_rust());
    assert_eq!(p.plan("cargo test"), full("full run for rust commands: Cargo.lock changed"));
    p.is("./gradlew test", "skip");
}

#[test]
fn kotlin_narrows_gradle_test_to_the_classes_a_change_reaches_per_project() {
    let p = Plans::new(&edit("core/src/main/kotlin/shop/text/Strings.kt"), &jvm_rust());
    let argv = p.narrow("./gradlew test");
    assert_eq!(argv.len(), 6, "{argv:?}");
    assert_eq!(argv[..2], ["./gradlew", "--init-script"]);
    assert_eq!(argv[3..], [":app:test", "--tests", "app.ShoutTest*"]);
    assert_eq!(p.narrow("ktlint"), ["ktlint", "core/src/main/kotlin/shop/text/Strings.kt"]);
    p.is("./gradlew detekt", "full");
    p.is("cargo test", "skip");
    p.is("cargo clippy", "skip");
}

#[test]
fn kotlin_selects_the_tests_that_used_a_deleted_class() {
    let p = Plans::new(&removed("core/src/main/kotlin/shop/Cart.kt"), &jvm_rust());
    let argv = p.narrow("./gradlew test");
    assert!(argv.contains(&":core:test".to_string()) && argv.contains(&"shop.CartTest*".to_string()), "{argv:?}");
}

#[test]
fn kotlin_runs_every_command_in_full_when_the_build_or_a_convention_plugin_changes() {
    let p = Plans::new(&files(&[("core/build.gradle.kts", "plugins { kotlin(\"jvm\") }\n")]), &jvm_rust());
    assert_eq!(p.plan("./gradlew test"), full("full run for kotlin commands: core/build.gradle.kts changed"));
    p.is("cargo test", "skip");

    let with_logic = cat(&[
        &jvm_rust(),
        &files(&[
            ("settings.gradle.kts", "pluginManagement { includeBuild(\"build-logic\") }\ninclude(\"core\", \"app\")\n"),
            ("build-logic/src/main/kotlin/conventions.kt", "package conventions\n"),
        ]),
    ]);
    let logic = Plans::new(&files(&[("build-logic/src/main/kotlin/conventions.kt", "package conventions\n// x\n")]), &with_logic);
    match logic.plan("./gradlew test") {
        Outcome::Full { note, .. } => assert!(note.contains("build-logic/src/main/kotlin/conventions.kt changed"), "{note}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn skips_rust_and_kotlin_commands_on_a_docs_only_change() {
    let p = Plans::new(&files(&[("docs/guide.md", "# guide\n")]), &jvm_rust());
    for cmd in ["cargo test", "cargo clippy", "./gradlew test", "./gradlew check", "ktlint"] {
        p.is(cmd, "skip");
    }
}
