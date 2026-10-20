mod common;

use common::*;
use picky::graph::{Graph, build_graph, php_referencers_of, transitive_dependents};
use picky::php::{declared_names, parse_php};

fn graph_of(f: &Files) -> Graph {
    let dir = temp_dir();
    write_files(&dir, f);
    build_graph(&dir, &f.keys().cloned().collect::<Vec<_>>()).unwrap()
}

fn deps(g: &Graph, file: &str) -> Vec<String> {
    sorted(g.imports.get(file).into_iter().flatten().cloned())
}

#[test]
fn reads_namespaces_declarations_imports_and_includes() {
    let php = parse_php(
        "src/Http/Kernel.php",
        &[
            "<?php",
            "namespace App\\Http;",
            "use App\\Models\\{User, Post as P};",
            "use Illuminate\\Support\\Str as S;",
            "use function App\\helpers\\money;",
            "require __DIR__ . '/routes.php';",
            "require_once dirname(__DIR__) . '/bootstrap.php';",
            "final class Kernel { use Concerns\\Boots; }",
        ]
        .join("\n"),
    );
    assert_eq!(php.classes, ["app\\http\\kernel"]);
    for name in ["app\\models\\user", "app\\models\\post", "illuminate\\support\\str", "app\\helpers\\money", "app\\http\\concerns\\boots"]
    {
        assert!(php.references.contains(name), "{name}"); // a trait resolves relative to the namespace
    }
    assert!(!php.references.contains("app\\http\\kernel"));
    assert_eq!(php.includes, ["src/Http/routes.php", "src/bootstrap.php"]);
}

#[test]
fn reads_statements_that_share_a_line_with_the_open_tag() {
    let php = parse_php("a.php", "<?php namespace App; use App\\Models\\User; final class Money { use HasCents; }");
    assert_eq!(php.classes, ["app\\money"]);
    assert!(php.references.contains("app\\models\\user"));
    assert!(php.references.contains("app\\hascents"));
}

#[test]
fn reads_statements_after_every_javascript_line_terminator() {
    // `\r`, U+2028 and U+2029 start a line for the TS regexes' `^`; after a comment, only a line start begins a statement.
    let php = parse_php(
        "src/a.php",
        "<?php // one\rnamespace App; // two\u{2028}use Lib\\Thing as T; /* three */\u{2029}final class Box // four\r\n{ use T; }\rrequire \"inc.php\";",
    );
    assert_eq!(php.classes, ["app\\box"]);
    assert!(!php.references.contains("t")); // `T` is the imported alias, not a global name
    assert_eq!(php.includes, ["src/inc.php", "inc.php"]);
}

#[test]
fn finds_class_names_in_docblocks_and_strings() {
    let php = parse_php("a.php", "<?php\nnamespace A;\n/** @return \\App\\Dto */\nfunction f() { return app('App\\\\Models\\\\User'); }\n");
    assert!(php.references.contains("app\\dto"));
    assert!(php.references.contains("app\\models\\user"));
}

#[test]
fn declares_global_functions_only_in_files_without_classes() {
    assert_eq!(declared_names("<?php\nfunction helper() {}\n"), ["helper"]);
    assert_eq!(declared_names("<?php\nnamespace A;\nclass B { function c() {} }\n"), ["a\\b"]);
}

#[test]
fn links_files_by_class_name_including_same_namespace_and_docblock_references() {
    let g = graph_of(&shop_php());
    assert_eq!(deps(&g, "app/Cart.php"), ["app/Money.php"]); // only a @var docblock names Money
    assert_eq!(deps(&g, "tests/Unit/CartTest.php"), ["app/Cart.php", "tests/TestCase.php"]);
    assert_eq!(deps(&g, "tests/Unit/UserTest.php"), ["app/Models/User.php", "tests/TestCase.php"]);
    assert_eq!(deps(&g, "tests/Unit/FormatTest.php"), ["app/helpers.php", "tests/TestCase.php"]);
}

#[test]
fn makes_every_pest_test_depend_on_its_pest_php_and_so_on_the_base_test_case_it_binds() {
    let g = graph_of(&shop_php());
    assert_eq!(deps(&g, "tests/Feature/HomeTest.php"), ["tests/Feature/Pest.php"]);
    assert_eq!(
        sorted(transitive_dependents(&g, &["tests/TestCase.php".to_string()])),
        [
            "tests/Feature/HomeTest.php",
            "tests/Feature/Pest.php",
            "tests/Unit/CartTest.php",
            "tests/Unit/FormatTest.php",
            "tests/Unit/UserTest.php"
        ]
    );
}

#[test]
fn links_tests_to_a_phpunit_xml_bootstrap_file_in_the_repo() {
    let g = graph_of(&cat(&[
        &shop_php(),
        &files(&[
            ("phpunit.xml", "<?xml version=\"1.0\"?>\n<phpunit bootstrap=\"tests/bootstrap.php\" colors=\"true\"></phpunit>"),
            ("tests/bootstrap.php", "<?php\n"),
        ]),
    ]));
    assert!(deps(&g, "tests/Unit/UserTest.php").contains(&"tests/bootstrap.php".to_string()));
}

#[test]
fn ignores_vendor_and_finds_who_referenced_a_removed_class() {
    let g = graph_of(&cat(&[&shop_php(), &files(&[("vendor/acme/lib/Cart.php", "<?php\nnamespace App;\nclass Cart {}\n")])]));
    assert!(!g.imports.contains_key("vendor/acme/lib/Cart.php"));
    let found: Vec<String> = php_referencers_of(&g, &["app\\models\\user".to_string()]).into_iter().collect();
    assert_eq!(found, ["tests/Unit/UserTest.php"]);
}
