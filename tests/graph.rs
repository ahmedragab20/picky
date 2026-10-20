mod common;

use common::*;
use picky::graph::{Graph, build_graph, importers_of_missing, transitive_dependents};

fn graph_of(entries: &[(&str, &str)]) -> Graph {
    let dir = temp_dir();
    write_files(&dir, &files(entries));
    let names: Vec<String> = entries.iter().map(|(f, _)| f.to_string()).collect();
    build_graph(&dir, &names).unwrap()
}

fn imports(g: &Graph, file: &str) -> Vec<String> {
    sorted(g.imports[file].iter().cloned())
}

#[test]
fn resolves_relative_imports_js_to_ts_index_files_re_exports_and_dynamic_imports() {
    let g = graph_of(&[
        ("src/a.ts", "import './b.js'; export * from './lib'; const c = await import('./c');"),
        ("src/b.ts", ""),
        ("src/lib/index.ts", ""),
        ("src/c.tsx", ""),
    ]);
    assert_eq!(imports(&g, "src/a.ts"), ["src/b.ts", "src/c.tsx", "src/lib/index.ts"]);
}

#[test]
fn resolves_tsconfig_paths_and_workspace_packages() {
    let g = graph_of(&[
        ("tsconfig.json", "{ // comments are fine\n \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@/*\": [\"src/*\"] } } }"),
        ("src/app.ts", "import '@/util'; import '@shop/core'; import '@shop/core/money'; import 'react';"),
        ("src/util.ts", ""),
        ("packages/core/package.json", r#"{ "name": "@shop/core", "main": "dist/index.js" }"#),
        ("packages/core/src/index.ts", ""),
        ("packages/core/money.ts", ""),
    ]);
    assert_eq!(imports(&g, "src/app.ts"), ["packages/core/money.ts", "packages/core/src/index.ts", "src/util.ts"]);
}

#[test]
fn finds_transitive_dependents() {
    let g = graph_of(&[
        ("src/math.ts", ""),
        ("src/cart.ts", "import './math.ts';"),
        ("test/cart.test.ts", "import '../src/cart.ts';"),
        ("test/other.test.ts", ""),
    ]);
    assert_eq!(sorted(transitive_dependents(&g, &["src/math.ts".to_string()])), ["src/cart.ts", "test/cart.test.ts"]);
}

#[test]
fn finds_files_that_still_import_a_removed_file() {
    let g = graph_of(&[("test/old.test.ts", "import '../src/gone.js';")]);
    let found: Vec<String> = importers_of_missing(&g, &["src/gone.ts".to_string()]).into_iter().collect();
    assert_eq!(found, ["test/old.test.ts"]);
}
