mod common;

use common::*;
use picky::commands::{Command, recognize};
use picky::graph::{Graph, build_graph, jvm_referencers_of, transitive_dependents};
use picky::kotlin::{Language, included_builds, parse_jvm, settings_includes, strip_code};

fn build(f: &Files) -> Dir {
    let dir = temp_dir();
    write_files(&dir, &cat(&[&files(&[(".git/HEAD", "ref: refs/heads/main\n")]), f]));
    dir
}

fn graph_of(f: &Files) -> Graph {
    build_graph(&build(f), &f.keys().cloned().collect::<Vec<_>>()).unwrap()
}

fn dependents(g: &Graph, file: &str) -> Vec<String> {
    sorted(transitive_dependents(g, &[file.to_string()]))
}

#[test]
fn strip_code_blanks_comments_and_strings_keeping_offsets_templates_and_nested_comments() {
    let text = "val a = \"x ${b[\"k\"]} // y\" /* c /* d */ e */ + 'q'\n// tail\nval z = \"\"\"raw \"quoted\" \"\"\"";
    let code = strip_code(text, Language::Kotlin, false);
    assert_eq!(code.encode_utf16().count(), text.encode_utf16().count());
    assert_eq!(code.split('\n').count(), 3);
    for gone in ["x", "y", "c", "e", "q", "tail", "raw", "quoted"] {
        assert!(!code.contains(gone), "{gone} in {code:?}");
    }
    assert!(code.contains("val a = \""));
    assert!(code.contains("val z = \"\"\""));
    let kts = "include(\"a\") // include(\"b\")";
    assert_eq!(strip_code(kts, Language::Kotlin, true), format!("{:<1$}", "include(\"a\")", kts.len()));
    let groovy = "include 'a' // 'b'";
    assert_eq!(strip_code(groovy, Language::Groovy, true), format!("{:<1$}", "include 'a'", groovy.len()));
}

#[test]
fn parse_jvm_reads_declarations_test_classes_and_the_names_a_kotlin_file_references() {
    let jvm = parse_jvm(
        "src/test/kotlin/shop/CartTest.kt",
        &[
            "@file:JvmName(\"CartTests\")",
            "package shop",
            "",
            "import shop.money.Money as Cash",
            "import shop.text.*",
            "import org.junit.jupiter.api.Test",
            "",
            "abstract class Base",
            "enum class Kind { A }",
            "sealed interface Shape",
            "typealias Id = Long",
            "",
            "class CartTest : Base() {",
            "    class Nested { fun inner() = 1 }",
            "    @Test fun totals() { Cash(1); shout(\"a\"); Class.forName(\"shop.repo.Store\") }",
            "}",
            "",
            "fun helper() = Kind.A",
            "val VERSION = 1",
        ]
        .join("\n"),
    );
    assert_eq!(jvm.package, "shop");
    assert_eq!(
        jvm.declarations,
        [
            "shop.Base",
            "shop.Kind",
            "shop.Shape",
            "shop.Id",
            "shop.CartTest",
            "shop.helper",
            "shop.VERSION",
            "shop.CartTestKt",
            "shop.CartTests"
        ]
    );
    assert_eq!(jvm.test_classes, ["shop.CartTest"]); // not abstract, enum, interface or nested
    assert!(jvm.references.contains("shop.money.Money")); // through the alias
    assert!(jvm.references.contains("shop.text.shout")); // through the wildcard
    assert!(jvm.references.contains("shop.repo.Store")); // a name in a string
    assert!(!jvm.references.contains("shop.CartTest"));
}

#[test]
fn parse_jvm_finds_test_classes_that_inherit_their_tests_and_java_classes() {
    assert_eq!(parse_jvm("a/FooSpec.kt", "package a\nclass FooSpec : FunSpec({ test(\"x\") {} })\n").test_classes, ["a.FooSpec"]);
    assert_eq!(parse_jvm("a/FooTest.kt", "package a\nclass FooTest : BaseTest()\n").test_classes, ["a.FooTest"]);
    assert!(parse_jvm("a/Helper.kt", "package a\nclass Helper\n").test_classes.is_empty());
    let java = parse_jvm(
        "a/FooTest.java",
        "package a;\nimport static org.junit.Assert.*;\npublic class FooTest {\n  @Test public void x() {}\n}\n",
    );
    assert_eq!(java.package, "a");
    assert_eq!(java.declarations, ["a.FooTest"]);
    assert_eq!(java.test_classes, ["a.FooTest"]);
}

#[test]
fn parse_jvm_reads_imports_on_lines_ending_in_every_javascript_line_terminator() {
    // `\r`, U+2028 and U+2029 end a line for the TS regex's `^` and `$`, and one terminator ends a line and starts the next.
    let jvm = parse_jvm("a/B.kt", "package p\rimport c.D as E\u{2028}import f.g.*\u{2029}val v = E.call(Zed)");
    assert!(jvm.references.contains("c.D.call")); // through the alias
    assert!(jvm.references.contains("f.g.Zed")); // through the wildcard
}

#[test]
fn parse_jvm_reads_a_type_after_a_class_literal_and_a_supertype_within_300_utf16_units() {
    // `Foo::class` is no declaration, and doesn't hide the one after it.
    assert_eq!(parse_jvm("a/B.kt", "package p\nval k = Foo::class\nclass Real").declarations, ["p.Real", "p.k", "p.BKt"]);
    // The header TS reads for a supertype is 300 UTF-16 units: an emoji counts two.
    let header = |emoji: usize| parse_jvm("a/B.kt", &format!("package p\nclass Wide {}:", "\u{1F600}".repeat(emoji))).test_classes;
    assert_eq!(header(149), ["p.Wide"]);
    assert!(header(150).is_empty());
}

#[test]
fn makes_test_classes_under_src_test_test_units() {
    assert_eq!(
        sorted(graph_of(&shop_kotlin()).test_units),
        ["app/src/test/java/app/ShoutTest.java", "core/src/test/kotlin/shop/CartTest.kt", "core/src/test/kotlin/shop/MoneyTest.kt"]
    );
}

#[test]
fn links_files_by_name_same_package_imports_java_calls_to_kotlin_facades_resources() {
    let g = graph_of(&shop_kotlin());
    assert_eq!(
        dependents(&g, "core/src/main/kotlin/shop/Money.kt"),
        ["core/src/main/kotlin/shop/Cart.kt", "core/src/test/kotlin/shop/CartTest.kt", "core/src/test/kotlin/shop/MoneyTest.kt"]
    );
    assert_eq!(
        dependents(&g, "core/src/main/kotlin/shop/text/Strings.kt"),
        ["app/src/main/kotlin/app/Main.kt", "app/src/test/java/app/ShoutTest.java"]
    );
    assert_eq!(
        dependents(&g, "core/src/test/resources/cart.json"),
        ["core/src/test/kotlin/shop/CartTest.kt", "core/src/test/kotlin/shop/MoneyTest.kt"]
    );
}

#[test]
fn finds_who_referenced_a_removed_declaration() {
    let g = graph_of(&shop_kotlin());
    assert_eq!(sorted(jvm_referencers_of(&g, &["shop.Cart".to_string()])), ["core/src/test/kotlin/shop/CartTest.kt"]);
}

#[test]
fn reads_literal_includes_in_kotlin_and_groovy_settings() {
    assert_eq!(
        settings_includes(
            "rootProject.name = \"x\"\ninclude(\"core\", \":app\")\ninclude(\":libs:db\") // include(\"old\")\n",
            Language::Kotlin
        ),
        Some(strings(&["core", ":app", ":libs:db"]))
    );
    assert_eq!(
        settings_includes("include 'core',\n  'app'\ninclude ':libs:db'\n", Language::Groovy),
        Some(strings(&["core", "app", ":libs:db"]))
    );
}

#[test]
fn gives_up_on_computed_projects() {
    assert_eq!(settings_includes("listOf(\"a\", \"b\").forEach { include(it) }\n", Language::Kotlin), None);
    assert_eq!(settings_includes("include(\"a\")\nproject(\":a\").projectDir = file(\"modules/a\")\n", Language::Kotlin), None);
    assert_eq!(settings_includes("include(\"$prefix-core\")\n", Language::Kotlin), None);
}

#[test]
fn finds_included_builds() {
    let dir = build(&files(&[("settings.gradle.kts", "pluginManagement {\n    includeBuild(\"build-logic\")\n}\ninclude(\"app\")\n")]));
    assert_eq!(included_builds(&dir, &strings(&["settings.gradle.kts"])), ["build-logic"]);
}

fn tests_command(argv: &[&str], cwd: &str) -> picky::commands::TestCommand {
    match recognize(&strings(argv), cwd).unwrap() {
        Some(Command::Tests(c)) => c,
        _ => panic!("{argv:?}: not a test command"),
    }
}

/// `{ kind, tool, ecosystem }` of a recognized command.
fn kind(argv: &[&str], cwd: &str) -> Option<(&'static str, String, &'static str)> {
    recognize(&strings(argv), cwd).unwrap().map(|c| match c {
        Command::Tests(t) => ("tests", t.tool, t.ecosystem.as_str()),
        Command::Files(f) => ("files", f.tool, f.ecosystem.as_str()),
        Command::Check(k) => ("check", k.tool, k.ecosystem.as_str()),
    })
}

#[test]
fn narrows_gradle_test_to_each_project_task_with_tests_filters() {
    let dir = build(&shop_kotlin());
    let c = tests_command(&["./gradlew", "test", "--no-daemon", "-Pci=true"], &dir);
    let argv = (c.narrow)(&strings(&[
        "core/src/test/kotlin/shop/MoneyTest.kt",
        "app/src/test/java/app/ShoutTest.java",
        "core/src/test/kotlin/shop/CartTest.kt",
    ]))
    .unwrap();
    assert_eq!(argv[..4], ["./gradlew", "--no-daemon", "-Pci=true", "--init-script"]);
    assert!(argv[4].ends_with("picky-1-no-matching-tests.init.gradle"), "{}", argv[4]);
    assert_eq!(
        argv[5..],
        [":app:test", "--tests", "app.ShoutTest*", ":core:test", "--tests", "shop.CartTest*", "--tests", "shop.MoneyTest*"]
    );
}

#[test]
fn addresses_the_root_project_as_test() {
    let dir = build(&files(&[
        ("build.gradle.kts", "plugins { kotlin(\"jvm\") }\n"),
        ("src/test/kotlin/a/ATest.kt", "package a\nclass ATest { @Test fun x() {} }\n"),
    ]));
    let argv = (tests_command(&["gradle", "test"], &dir).narrow)(&strings(&["src/test/kotlin/a/ATest.kt"])).unwrap();
    assert_eq!(argv.len(), 6);
    assert_eq!(argv[..2], ["gradle", "--init-script"]);
    assert_eq!(argv[3..], [":test", "--tests", "a.ATest*"]);
}

#[test]
fn runs_android_and_kotlin_multiplatform_projects_in_full() {
    let dir = build(&cat(&[&shop_kotlin(), &files(&[("app/build.gradle.kts", "plugins { id(\"com.android.application\") }\n")])]));
    let c = tests_command(&["./gradlew", "test"], &dir);
    assert_eq!(c.why_not.as_deref(), Some("Android and Kotlin Multiplatform test tasks take no --tests filter"));
    assert_eq!((c.narrow)(&strings(&["app/src/test/java/app/ShoutTest.java"])), None);
    assert!((c.narrow)(&strings(&["core/src/test/kotlin/shop/MoneyTest.kt"])).is_some());
}

#[test]
fn only_owns_test_files_of_the_builds_projects() {
    let dir = build(&shop_kotlin());
    let c = tests_command(&["./gradlew", "test"], &dir);
    let owns = c.owns.unwrap();
    assert!(owns("core/src/test/kotlin/shop/MoneyTest.kt"));
    assert!(!owns("tools/src/test/kotlin/ToolTest.kt")); // not an included project
    assert_eq!(kind(&["../gradlew", "test"], &format!("{}/core", &*dir)).map(|k| k.0), Some("tests"));
}

#[test]
fn treats_verification_tasks_as_checks_and_leaves_other_invocations_alone() {
    let dir = build(&shop_kotlin());
    assert_eq!(kind(&["./gradlew", "check"], &dir), Some(("check", "gradle check".into(), "kotlin")));
    assert_eq!(
        kind(&["./gradlew", "detekt", ":core:ktlintCheck"], &dir).map(|k| (k.0, k.1)),
        Some(("check", "gradle detekt :core:ktlintCheck".into()))
    );
    for argv in [
        &["./gradlew", "test", "--tests", "shop.MoneyTest"][..],
        &["./gradlew", ":core:test"],
        &["./gradlew", "test", "build"],
        &["./gradlew", "build"],
        &["./gradlew", "-p", "core", "test"],
        &["./gradlew", "test", "--some-new-flag"],
    ] {
        assert_eq!(kind(argv, &dir), None, "{argv:?}");
    }
    let computed =
        build(&cat(&[&shop_kotlin(), &files(&[("settings.gradle.kts", "file(\"modules\").listFiles().forEach { include(it.name) }\n")])]));
    assert_eq!(kind(&["./gradlew", "test"], &computed), None);
}

#[test]
fn narrows_ktlint_without_patterns_to_the_changed_kotlin_files() {
    let Some(Command::Files(c)) = recognize(&strings(&["ktlint", "--reporter", "plain", "--relative"]), &temp_dir()).unwrap() else {
        panic!("not a files command")
    };
    assert_eq!(c.tool, "ktlint");
    assert_eq!(c.targets, ["."]);
    assert!(!c.with_dependents);
    assert_eq!((c.narrow)(&strings(&["a/B.kt"])), ["ktlint", "--reporter", "plain", "--relative", "a/B.kt"]);
    assert_eq!(kind(&["ktlint", "src/**/*.kt", "!**/generated/**"], &temp_dir()), Some(("check", "ktlint".into(), "kotlin")));
}

#[test]
fn treats_detekt_as_a_check() {
    assert_eq!(kind(&["detekt", "--build-upon-default-config"], &temp_dir()), Some(("check", "detekt".into(), "kotlin")));
}
