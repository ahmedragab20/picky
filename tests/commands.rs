mod common;

use common::*;
use picky::commands::{Command, recognize};

const FILES: [&str; 2] = ["test/a.test.ts", "test/b.test.ts"];

fn argv(words: &[&str]) -> Vec<String> {
    strings(words)
}

fn with_files(words: &[&str]) -> Vec<String> {
    let mut v = argv(words);
    v.extend(FILES.iter().map(|f| f.to_string()));
    v
}

fn narrowed_in(cmd: &[&str], cwd: &str) -> Option<Vec<String>> {
    let files = strings(&FILES);
    match recognize(&argv(cmd), cwd).unwrap()? {
        Command::Tests(c) => (c.narrow)(&files),
        Command::Files(c) => Some((c.narrow)(&files)),
        Command::Check(_) => None,
    }
}

fn narrowed(cmd: &[&str]) -> Option<Vec<String>> {
    narrowed_in(cmd, &temp_dir())
}

/// `{ kind, tool, ecosystem }` of a recognized command.
fn kind(cmd: &[&str], cwd: &str) -> Option<(&'static str, String, &'static str)> {
    recognize(&argv(cmd), cwd).unwrap().map(|c| match c {
        Command::Tests(t) => ("tests", t.tool, t.ecosystem.as_str()),
        Command::Files(f) => ("files", f.tool, f.ecosystem.as_str()),
        Command::Check(k) => ("check", k.tool, k.ecosystem.as_str()),
    })
}

fn check(tool: &str, ecosystem: &'static str) -> Option<(&'static str, String, &'static str)> {
    Some(("check", tool.to_string(), ecosystem))
}

#[test]
fn narrows_vitest_keeping_launchers_sub_commands_and_flags() {
    assert_eq!(narrowed(&["npx", "-y", "vitest", "run", "--coverage"]), Some(with_files(&["npx", "-y", "vitest", "run", "--coverage"])));
    assert_eq!(narrowed(&["./node_modules/.bin/vitest"]), Some(with_files(&["./node_modules/.bin/vitest"])));
    assert_eq!(narrowed(&["pnpm", "exec", "vitest", "run"]), Some(with_files(&["pnpm", "exec", "vitest", "run"])));
}

#[test]
fn narrows_jest_by_exact_path_and_node_test_by_file() {
    assert_eq!(narrowed(&["jest", "--ci"]), Some(with_files(&["jest", "--ci", "--runTestsByPath"])));
    assert_eq!(narrowed(&["node", "--test"]), Some(with_files(&["node", "--test"])));
}

#[test]
fn keeps_environment_assignments_in_front() {
    assert_eq!(narrowed(&["CI=1", "vitest", "run"]), Some(with_files(&["CI=1", "vitest", "run"])));
}

#[test]
fn leaves_commands_alone_when_they_already_target_tests_or_are_unknown() {
    assert_eq!(narrowed(&["vitest", "run", "src/only"]), None);
    assert_eq!(narrowed(&["vitest", "--reporter", "dot"]), None); // value could be a filter
    assert_eq!(narrowed(&["node", "script.js"]), None);
    assert_eq!(narrowed(&["make", "test"]), None);
}

#[test]
fn resolves_package_scripts_that_run_a_known_runner() {
    let cwd = temp_dir();
    write_files(
        &cwd,
        &files(&[("package.json", r#"{"scripts":{"test":"vitest run","unit":"npm test","lint":"eslint .","e2e":"vitest run && echo"}}"#)]),
    );
    assert_eq!(narrowed_in(&["npm", "test", "--silent"], &cwd), Some(with_files(&["npm", "--silent", "run", "test", "--"])));
    assert_eq!(narrowed_in(&["npm", "run", "unit"], &cwd), Some(with_files(&["npm", "run", "unit", "--"])));
    assert_eq!(narrowed_in(&["pnpm", "run", "test"], &cwd), Some(with_files(&["pnpm", "run", "test"])));
    assert_eq!(narrowed_in(&["yarn", "test"], &cwd), Some(with_files(&["yarn", "run", "test"])));
    assert_eq!(narrowed_in(&["npm", "run", "e2e"], &cwd), None);
    assert_eq!(narrowed_in(&["npm", "run", "missing"], &cwd), None);
}

fn lock(version: &str) -> Files {
    let text = format!(r#"{{"packages":[],"packages-dev":[{{"name":"phpunit/phpunit","version":"{version}"}}]}}"#);
    files(&[("composer.lock", &text)])
}

#[test]
fn runs_phpunit_and_pest_on_several_paths_only_from_version_10() {
    let modern = temp_dir();
    write_files(&modern, &lock("11.5.3"));
    assert_eq!(
        narrowed_in(&["vendor/bin/phpunit", "--colors=never"], &modern),
        Some(with_files(&["vendor/bin/phpunit", "--colors=never"]))
    );
    assert_eq!(
        narrowed_in(&["php", "-d", "memory_limit=-1", "vendor/bin/pest"], &modern),
        Some(with_files(&["php", "-d", "memory_limit=-1", "vendor/bin/pest"]))
    );
    assert!(recognize(&argv(&["phpunit", "--testsuite=unit"]), &modern).unwrap().is_none());

    let old = temp_dir();
    write_files(&old, &lock("9.6.38"));
    let Some(Command::Tests(c)) = recognize(&argv(&["vendor/bin/phpunit"]), &old).unwrap() else { panic!("not a test command") };
    assert_eq!((c.narrow)(&strings(&["tests/ATest.php"])), Some(argv(&["vendor/bin/phpunit", "tests/ATest.php"])));
    assert_eq!((c.narrow)(&strings(&FILES)), None); // 9.x would silently run only the first path
}

#[test]
fn resolves_composer_scripts_and_always_forwards_paths_after_double_dash() {
    let cwd = temp_dir();
    write_files(
        &cwd,
        &files(&[
            ("composer.lock", r#"{"packages-dev":[{"name":"phpunit/phpunit","version":"11.0.0"}]}"#),
            (
                "composer.json",
                r#"{"scripts":{"test":"@php vendor/bin/phpunit","unit":["phpunit"],"ci":"@test","stan":"phpstan analyse","both":["phpunit","pint"]}}"#,
            ),
        ]),
    );
    assert_eq!(narrowed_in(&["composer", "test"], &cwd), Some(with_files(&["composer", "run-script", "test", "--"])));
    assert_eq!(narrowed_in(&["composer", "-n", "run", "unit"], &cwd), Some(with_files(&["composer", "-n", "run-script", "unit", "--"])));
    assert_eq!(narrowed_in(&["composer", "ci"], &cwd), Some(with_files(&["composer", "run-script", "ci", "--"])));
    assert_eq!(kind(&["composer", "stan"], &cwd), check("phpstan", "php"));
    assert_eq!(kind(&["composer", "both"], &cwd), None);
    assert_eq!(kind(&["composer", "install"], &cwd), None);
}

#[test]
fn treats_type_checkers_and_static_analyzers_as_whole_project_checks() {
    for (cmd, tool, ecosystem) in [
        (&["npx", "tsc", "--noEmit"][..], "tsc", "js"),
        (&["vue-tsc", "-b"][..], "vue-tsc", "js"),
        (&["vendor/bin/phpstan", "analyse", "src"][..], "phpstan", "php"),
        (&["php", "vendor/bin/php-cs-fixer", "fix", "--dry-run"][..], "php-cs-fixer", "php"),
    ] {
        assert_eq!(kind(cmd, &temp_dir()), check(tool, ecosystem), "{cmd:?}");
    }
}

#[test]
fn narrows_eslint_with_a_flat_config_and_keeps_ignored_files_silent() {
    let cwd = temp_dir();
    write_files(&cwd, &files(&[("eslint.config.js", "export default [];")]));
    let Some(Command::Files(c)) = recognize(&argv(&["npx", "eslint", "--max-warnings", "0", "src", "test"]), &cwd).unwrap() else {
        panic!("not a files command")
    };
    assert_eq!(c.targets, ["src", "test"]);
    assert!(c.with_dependents);
    assert_eq!((c.narrow)(&strings(&["src/a.ts"])), ["npx", "eslint", "--max-warnings", "0", "--no-warn-ignored", "src/a.ts"]);
    let Some(Command::Files(bare)) = recognize(&argv(&["eslint"]), &cwd).unwrap() else { panic!("not a files command") };
    assert_eq!(bare.targets, ["."]);
}

#[test]
fn runs_eslint_whole_when_narrowing_could_change_its_result() {
    let legacy = temp_dir();
    write_files(&legacy, &files(&[(".eslintrc.json", "{}")]));
    assert_eq!(kind(&["eslint", "."], &legacy), check("eslint", "js"));

    let cross_file = temp_dir();
    write_files(&cross_file, &files(&[("eslint.config.js", "export default [{ rules: { 'import/no-unused-modules': 'error' } }];")]));
    assert_eq!(kind(&["eslint", "."], &cross_file).map(|k| k.0), Some("check"));

    let flat = temp_dir();
    write_files(&flat, &files(&[("eslint.config.js", "export default [];")]));
    assert_eq!(kind(&["eslint", "--some-new-flag", "value", "."], &flat), None); // unknown option: value or path?
}

#[test]
fn narrows_prettier_adding_ignore_unknown() {
    let Some(Command::Files(c)) = recognize(&argv(&["prettier", "--check", "."]), &temp_dir()).unwrap() else { panic!() };
    assert_eq!((c.narrow)(&strings(&["a.md"])), ["prettier", "--check", "--ignore-unknown", "a.md"]);
}

#[test]
fn runs_a_narrowed_lint_script_through_the_local_binary_not_the_package_manager() {
    let cwd = temp_dir();
    write_files(
        &cwd,
        &files(&[
            ("eslint.config.js", "export default [];"),
            ("node_modules/.bin/eslint", ""),
            ("package.json", r#"{"scripts":{"lint":"eslint .","prelinted":"x","fmt":"prettier --check .","prefmt":"echo"}}"#),
        ]),
    );
    let Some(Command::Files(c)) = recognize(&argv(&["CI=1", "npm", "run", "lint"]), &cwd).unwrap() else { panic!() };
    assert_eq!(
        (c.narrow)(&strings(&["a.ts"])),
        ["CI=1".to_string(), format!("{}/node_modules/.bin/eslint", &*cwd), "--no-warn-ignored".into(), "a.ts".into()]
    );
    assert_eq!(kind(&["npm", "run", "fmt"], &cwd), None); // a pre-script would be skipped
}
