mod common;

use common::*;
use picky::commands::{Command, recognize};
use picky::graph::{Graph, build_graph, importers_of_missing, transitive_dependents};
use picky::rust::{Build, Dependency, cargo_scope, parse_manifest, source_includes};

fn workspace(f: &Files) -> Dir {
    let dir = temp_dir();
    write_files(&dir, &cat(&[&files(&[(".git/HEAD", "ref: refs/heads/main\n")]), f]));
    dir
}

fn graph_of(f: &Files) -> Graph {
    build_graph(&workspace(f), &f.keys().cloned().collect::<Vec<_>>()).unwrap()
}

fn dependents(g: &Graph, file: &str) -> Vec<String> {
    sorted(transitive_dependents(g, &[file.to_string()]))
}

fn dep(name: &str, package: Option<&str>, path: Option<&str>, workspace: Option<bool>) -> Dependency {
    Dependency { name: name.into(), package: package.map(Into::into), path: path.map(Into::into), workspace }
}

#[test]
fn parse_manifest_reads_the_package_workspace_members_and_every_kind_of_dependency() {
    let m = parse_manifest(
        &[
            "[package]",
            "name = \"api\" # the service",
            "build = \"build/main.rs\"",
            "",
            "[workspace]",
            "members = [",
            "  \"crates/*\",  # all crates",
            "  'tools/gen',",
            "]",
            "exclude = [\"crates/legacy\"]",
            "",
            "[dependencies]",
            "core = { path = \"../core\", features = [\"x\"] }",
            "renamed = { package = \"money\", path = \"../money\" }",
            "shared.workspace = true",
            "serde = \"1\"",
            "",
            "[dev-dependencies.fixtures]",
            "path = \"fixtures\"",
            "",
            "[target.'cfg(unix)'.dependencies]",
            "unix-only = { path = \"../unix\" }",
            "",
            "[workspace.dependencies]",
            "shared = { path = \"crates/shared\" }",
        ]
        .join("\n"),
    );
    assert_eq!(m.name.as_deref(), Some("api"));
    assert_eq!(m.build, Build::Path("build/main.rs".into()));
    assert!(m.autotests);
    let ws = m.workspace.unwrap();
    assert_eq!(ws.members, ["crates/*", "tools/gen"]);
    assert_eq!(ws.exclude, ["crates/legacy"]);
    assert_eq!(ws.default_members, None);
    assert_eq!(ws.dependencies.get("shared"), Some(&dep("shared", None, Some("crates/shared"), None)));
    assert_eq!(
        m.dependencies,
        [
            dep("core", None, Some("../core"), None),
            dep("renamed", Some("money"), Some("../money"), None),
            dep("shared", None, None, Some(true)),
            dep("serde", None, None, None),
            dep("fixtures", None, Some("fixtures"), None),
            dep("unix-only", None, Some("../unix"), None),
        ]
    );
}

#[test]
fn marks_integration_test_names_unreliable_with_test_targets_or_autotests_false() {
    assert!(!parse_manifest("[package]\nname = \"a\"\n\n[[test]]\nname = \"it\"\npath = \"it/main.rs\"\n").autotests);
    assert!(!parse_manifest("[package]\nname = \"a\"\nautotests = false\n").autotests);
    let virtual_manifest = parse_manifest("[workspace]\nmembers = []\n");
    assert_eq!(virtual_manifest.name, None);
    assert_eq!(virtual_manifest.build, Build::Default);
    assert_eq!(parse_manifest("[package]\nname = \"a\"\nbuild = false\n").build, Build::Off);
}

#[test]
fn source_includes_resolves_include_macros_manifest_dir_paths_and_path_modules() {
    let text = [
        "#![doc = include_str!(\"../README.md\")]",
        "const SCHEMA: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/schema.sql\"));",
        "static LOGO: &[u8] = include_bytes!(\"assets/logo.png\");",
        "include!(\"generated.rs\");",
        "static DIR: Dir = include_dir!(\"$CARGO_MANIFEST_DIR/templates\");",
        "static M: Migrator = sqlx::migrate!(\"./migrations\");",
        "#[path = \"platform/unix.rs\"]",
        "mod platform;",
        "const OUTSIDE: &str = include_str!(\"../../../../etc/passwd\");",
    ]
    .join("\n");
    assert_eq!(
        source_includes("crates/api", "crates/api/src/lib.rs", &text),
        [
            "crates/api/README.md",
            "crates/api/schema.sql",
            "crates/api/src/assets/logo.png",
            "crates/api/src/generated.rs",
            "crates/api/templates",
            "crates/api/migrations",
            "crates/api/src/platform/unix.rs",
        ]
    );
}

#[test]
fn makes_each_crate_and_integration_test_target_a_test_unit() {
    assert_eq!(
        sorted(graph_of(&shop_rust()).test_units),
        [
            "crates/cart/Cargo.toml",
            "crates/money/Cargo.toml",
            "crates/money/tests/arith.rs",
            "crates/money/tests/format.rs",
            "crates/strings/Cargo.toml",
        ]
    );
}

#[test]
fn links_a_crate_to_its_sources_its_included_files_and_the_crates_it_depends_on() {
    let g = graph_of(&shop_rust());
    assert_eq!(
        dependents(&g, "crates/money/src/fmt.rs"),
        ["crates/cart/Cargo.toml", "crates/money/Cargo.toml", "crates/money/tests/arith.rs", "crates/money/tests/format.rs"]
    );
    // The README is the crate's doctest.
    assert!(dependents(&g, "crates/money/README.md").contains(&"crates/money/Cargo.toml".to_string()));
    assert_eq!(dependents(&g, "crates/strings/src/lib.rs"), ["crates/strings/Cargo.toml"]);
}

#[test]
fn runs_only_the_integration_targets_a_test_helper_reaches() {
    let g = graph_of(&shop_rust());
    assert_eq!(dependents(&g, "crates/money/tests/common/mod.rs"), ["crates/money/tests/arith.rs", "crates/money/tests/format.rs"]);
    assert!(dependents(&g, "crates/money/tests/arith.rs").is_empty());
}

#[test]
fn links_tests_helpers_to_the_crate_when_test_targets_make_target_names_unreliable() {
    let manifest =
        format!("{}\n[[test]]\nname = \"arith\"\npath = \"tests/arith.rs\"\n", shop_rust()["crates/money/Cargo.toml"].clone().unwrap());
    let g = graph_of(&cat(&[&shop_rust(), &files(&[("crates/money/Cargo.toml", &manifest)])]));
    assert!(!g.test_units.contains(&"crates/money/tests/arith.rs".to_string()));
    assert!(dependents(&g, "crates/money/tests/common/mod.rs").contains(&"crates/money/Cargo.toml".to_string()));
}

#[test]
fn resolves_workspace_inherited_and_renamed_dependencies() {
    let g = graph_of(&cat(&[
        &shop_rust(),
        &files(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.dependencies]\nmoney = { path = \"crates/money\" }\n"),
            (
                "crates/strings/Cargo.toml",
                "[package]\nname = \"strings\"\n\n[dependencies]\ncash = { package = \"money\", version = \"0.1\" }\nmoney.workspace = true\n",
            ),
        ]),
    ]));
    assert!(dependents(&g, "crates/money/src/lib.rs").contains(&"crates/strings/Cargo.toml".to_string()));
}

#[test]
fn counts_a_build_script_and_the_files_it_reads_as_crate_inputs() {
    let base = cat(&[
        &shop_rust(),
        &files(&[("crates/strings/proto/api.proto", "syntax = \"proto3\";\n"), ("crates/strings/data/words.txt", "a\n")]),
    ]);
    let listed = graph_of(&cat(&[
        &base,
        &files(&[(
            "crates/strings/build.rs",
            "fn main() {\n    println!(\"cargo::rerun-if-changed=data/words.txt\");\n    prost_build::compile_protos(&[\"proto/api.proto\"], &[\"proto\"]).unwrap();\n}\n",
        )]),
    ]));
    for file in ["crates/strings/build.rs", "crates/strings/data/words.txt", "crates/strings/proto/api.proto"] {
        assert_eq!(dependents(&listed, file), ["crates/strings/Cargo.toml"], "{file}");
    }

    // Without rerun-if-changed, cargo reruns the script on any package change: every file counts but docs.
    let unlisted = graph_of(&cat(&[&base, &files(&[("crates/strings/build.rs", "fn main() {}\n"), ("crates/strings/NOTES.md", "x\n")])]));
    assert_eq!(dependents(&unlisted, "crates/strings/data/words.txt"), ["crates/strings/Cargo.toml"]);
    assert!(dependents(&unlisted, "crates/strings/NOTES.md").is_empty());
}

#[test]
fn records_an_include_of_a_removed_file_as_unresolved() {
    let mut f = shop_rust();
    f.shift_remove("crates/money/README.md");
    let g = graph_of(&f);
    let found: Vec<String> = importers_of_missing(&g, &["crates/money/README.md".to_string()]).into_iter().collect();
    assert_eq!(found, ["crates/money/src/lib.rs"]);
}

fn narrow(argv: &[&str], cwd: &str, files: &[&str]) -> Vec<String> {
    match recognize(&strings(argv), cwd).unwrap() {
        Some(Command::Tests(c)) => (c.narrow)(&strings(files)).unwrap(),
        _ => panic!("{argv:?}: not a test command"),
    }
}

#[test]
fn narrows_cargo_test_to_p_per_crate_and_test_per_integration_target() {
    let ws = workspace(&shop_rust());
    let lib = ["crates/money/Cargo.toml", "crates/cart/Cargo.toml"];
    assert_eq!(
        narrow(&["cargo", "test", "--workspace", "--locked"], &ws, &lib),
        ["cargo", "test", "--locked", "-p", "cart", "-p", "money"]
    );
    assert_eq!(
        narrow(&["cargo", "+nightly", "test", "--all-features"], &ws, &["crates/money/tests/format.rs", "crates/money/tests/arith.rs"]),
        ["cargo", "+nightly", "test", "--all-features", "-p", "money", "--test", "arith", "--test", "format"]
    );
    // `--test` would apply to every listed package, so one whole crate makes every crate whole.
    assert_eq!(
        narrow(&["cargo", "test"], &ws, &["crates/money/tests/arith.rs", "crates/cart/Cargo.toml"]),
        ["cargo", "test", "-p", "cart", "-p", "money"]
    );
    // Target kinds select more than --test would: crates run whole.
    assert_eq!(
        narrow(&["cargo", "test", "--all-targets"], &ws, &["crates/money/tests/arith.rs"]),
        ["cargo", "test", "--all-targets", "-p", "money"]
    );
    assert_eq!(
        narrow(&["cargo", "test", "--", "--nocapture"], &ws, &["crates/money/tests/arith.rs"]),
        ["cargo", "test", "-p", "money", "--test", "arith", "--", "--nocapture"]
    );
}

#[test]
fn narrows_cargo_nextest_passing_a_run_that_selects_no_tests() {
    let ws = workspace(&shop_rust());
    assert_eq!(
        narrow(&["cargo", "nextest", "run", "--workspace", "-P", "ci"], &ws, &["crates/strings/Cargo.toml"]),
        ["cargo", "nextest", "run", "-P", "ci", "--no-tests=pass", "-p", "strings"]
    );
    assert_eq!(
        narrow(&["cargo", "nextest", "run", "--no-tests", "warn"], &ws, &["crates/strings/Cargo.toml"]),
        ["cargo", "nextest", "run", "--no-tests", "warn", "-p", "strings"]
    );
}

#[test]
fn follows_cargo_package_selection_default_members_a_member_directory_exclude() {
    let ws = workspace(&cat(&[
        &shop_rust(),
        &files(&[("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\ndefault-members = [\"crates/money\"]\n")]),
    ]));
    let at = |p: &str| if p.is_empty() { ws.to_string() } else { format!("{}/{p}", &*ws) };
    let owns = |argv: &[&str], cwd: &str, file: &str| match recognize(&strings(argv), cwd).unwrap() {
        Some(Command::Tests(c)) => (c.owns.unwrap())(&picky::npath::relative(cwd, &at(file))),
        _ => false,
    };
    assert!(owns(&["cargo", "test"], &ws, "crates/money/Cargo.toml"));
    assert!(!owns(&["cargo", "test"], &ws, "crates/cart/Cargo.toml"));
    assert!(owns(&["cargo", "test", "--workspace"], &ws, "crates/cart/Cargo.toml"));
    assert!(!owns(&["cargo", "test", "--workspace", "--exclude", "cart"], &ws, "crates/cart/Cargo.toml"));
    assert!(owns(&["cargo", "test"], &at("crates/cart"), "crates/cart/Cargo.toml"));
    assert!(!owns(&["cargo", "test"], &at("crates/cart"), "crates/money/Cargo.toml"));
}

#[test]
fn leaves_commands_that_already_select_tests_or_reach_beyond_the_cwd_alone() {
    let ws = workspace(&shop_rust());
    for argv in [
        &["cargo", "test", "adds"][..], // a name filter
        &["cargo", "test", "--", "adds"],
        &["cargo", "test", "-p", "money"],
        &["cargo", "test", "--lib"],
        &["cargo", "test", "--test", "arith"],
        &["cargo", "test", "--exclude", "cart"], // --exclude needs --workspace
        &["cargo", "test", "--some-new-flag"],
        &["cargo", "bench"],
    ] {
        assert!(recognize(&strings(argv), &ws).unwrap().is_none(), "{argv:?}");
    }
    // tests crates outside the cwd
    assert!(recognize(&strings(&["cargo", "test", "--workspace"]), &format!("{}/crates/cart", &*ws)).unwrap().is_none());
    assert!(recognize(&strings(&["cargo", "test"]), &temp_dir()).unwrap().is_none()); // no Cargo.toml
}

#[test]
fn treats_clippy_check_and_fmt_as_whole_workspace_checks() {
    for sub in ["clippy", "check", "fmt"] {
        match recognize(&strings(&["cargo", sub, "--all-targets"]), &temp_dir()).unwrap() {
            Some(Command::Check(c)) => {
                assert_eq!(c.tool, format!("cargo {sub}"));
                assert_eq!(c.ecosystem.as_str(), "rust");
            }
            _ => panic!("cargo {sub}: not a check"),
        }
    }
}

#[test]
fn cargo_scope_is_none_for_a_virtual_manifest_run_from_a_member_directory_with_workspace() {
    let ws = workspace(&shop_rust());
    assert!(cargo_scope(&format!("{}/crates/money", &*ws), true, &[]).is_none());
    assert!(cargo_scope(&ws, false, &[]).is_some());
}
