import { describe, expect, it } from 'bun:test';
import path from 'node:path';
import { recognize } from '../src/commands.ts';
import { buildGraph, importersOfMissing, transitiveDependents } from '../src/graph.ts';
import { cargoScope, parseManifest, sourceIncludes } from '../src/rust.ts';
import { SHOP_RUST, tempDir, writeFiles } from './helpers.ts';

function workspace(files: Record<string, string> = SHOP_RUST) {
  const dir = tempDir();
  writeFiles(dir, { '.git/HEAD': 'ref: refs/heads/main\n', ...files });
  return dir;
}

function graphOf(files: Record<string, string>) {
  const dir = workspace(files);
  return buildGraph(dir, Object.keys(files));
}

const dependents = (g: ReturnType<typeof graphOf>, file: string) => [...transitiveDependents(g, [file])].sort();

describe('parseManifest', () => {
  it('reads the package, workspace members and every kind of dependency', () => {
    const m = parseManifest(
      [
        '[package]',
        'name = "api" # the service',
        'build = "build/main.rs"',
        '',
        '[workspace]',
        'members = [',
        '  "crates/*",  # all crates',
        "  'tools/gen',",
        ']',
        'exclude = ["crates/legacy"]',
        '',
        '[dependencies]',
        'core = { path = "../core", features = ["x"] }',
        'renamed = { package = "money", path = "../money" }',
        'shared.workspace = true',
        'serde = "1"',
        '',
        '[dev-dependencies.fixtures]',
        'path = "fixtures"',
        '',
        "[target.'cfg(unix)'.dependencies]",
        'unix-only = { path = "../unix" }',
        '',
        '[workspace.dependencies]',
        'shared = { path = "crates/shared" }',
      ].join('\n'),
    );
    expect(m.name).toBe('api');
    expect(m.build).toBe('build/main.rs');
    expect(m.autotests).toBe(true);
    expect(m.workspace).toMatchObject({ members: ['crates/*', 'tools/gen'], exclude: ['crates/legacy'], defaultMembers: null });
    expect(m.workspace?.dependencies.get('shared')).toEqual({ name: 'shared', path: 'crates/shared' });
    expect(m.dependencies).toEqual([
      { name: 'core', path: '../core' },
      { name: 'renamed', package: 'money', path: '../money' },
      { name: 'shared', workspace: true },
      { name: 'serde' },
      { name: 'fixtures', path: 'fixtures' },
      { name: 'unix-only', path: '../unix' },
    ]);
  });

  it('marks integration test names unreliable with [[test]] targets or autotests = false', () => {
    expect(parseManifest('[package]\nname = "a"\n\n[[test]]\nname = "it"\npath = "it/main.rs"\n').autotests).toBe(false);
    expect(parseManifest('[package]\nname = "a"\nautotests = false\n').autotests).toBe(false);
    expect(parseManifest('[workspace]\nmembers = []\n')).toMatchObject({ name: null, build: null });
    expect(parseManifest('[package]\nname = "a"\nbuild = false\n').build).toBe(false);
  });
});

describe('sourceIncludes', () => {
  it('resolves include macros, CARGO_MANIFEST_DIR paths and #[path] modules', () => {
    const text = [
      '#![doc = include_str!("../README.md")]',
      'const SCHEMA: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/schema.sql"));',
      'static LOGO: &[u8] = include_bytes!("assets/logo.png");',
      'include!("generated.rs");',
      'static DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/templates");',
      'static M: Migrator = sqlx::migrate!("./migrations");',
      '#[path = "platform/unix.rs"]',
      'mod platform;',
      'const OUTSIDE: &str = include_str!("../../../../etc/passwd");',
    ].join('\n');
    expect(sourceIncludes('crates/api', 'crates/api/src/lib.rs', text)).toEqual([
      'crates/api/README.md',
      'crates/api/schema.sql',
      'crates/api/src/assets/logo.png',
      'crates/api/src/generated.rs',
      'crates/api/templates',
      'crates/api/migrations',
      'crates/api/src/platform/unix.rs',
    ]);
  });
});

describe('buildGraph: Rust', () => {
  it('makes each crate and integration test target a test unit', () => {
    expect(graphOf(SHOP_RUST).testUnits.sort()).toEqual([
      'crates/cart/Cargo.toml',
      'crates/money/Cargo.toml',
      'crates/money/tests/arith.rs',
      'crates/money/tests/format.rs',
      'crates/strings/Cargo.toml',
    ]);
  });

  it('links a crate to its sources, its included files and the crates it depends on', () => {
    const g = graphOf(SHOP_RUST);
    expect(dependents(g, 'crates/money/src/fmt.rs')).toEqual([
      'crates/cart/Cargo.toml',
      'crates/money/Cargo.toml',
      'crates/money/tests/arith.rs',
      'crates/money/tests/format.rs',
    ]);
    // The README is the crate's doctest.
    expect(dependents(g, 'crates/money/README.md')).toContain('crates/money/Cargo.toml');
    expect(dependents(g, 'crates/strings/src/lib.rs')).toEqual(['crates/strings/Cargo.toml']);
  });

  it('runs only the integration targets a test helper reaches', () => {
    const g = graphOf(SHOP_RUST);
    expect(dependents(g, 'crates/money/tests/common/mod.rs')).toEqual(['crates/money/tests/arith.rs', 'crates/money/tests/format.rs']);
    expect(dependents(g, 'crates/money/tests/arith.rs')).toEqual([]);
  });

  it('links tests/ helpers to the crate when [[test]] targets make target names unreliable', () => {
    const manifest = `${SHOP_RUST['crates/money/Cargo.toml']}\n[[test]]\nname = "arith"\npath = "tests/arith.rs"\n`;
    const g = graphOf({ ...SHOP_RUST, 'crates/money/Cargo.toml': manifest });
    expect(g.testUnits).not.toContain('crates/money/tests/arith.rs');
    expect(dependents(g, 'crates/money/tests/common/mod.rs')).toContain('crates/money/Cargo.toml');
  });

  it('resolves workspace-inherited and renamed dependencies', () => {
    const g = graphOf({
      ...SHOP_RUST,
      'Cargo.toml': '[workspace]\nmembers = ["crates/*"]\n\n[workspace.dependencies]\nmoney = { path = "crates/money" }\n',
      'crates/strings/Cargo.toml':
        '[package]\nname = "strings"\n\n[dependencies]\ncash = { package = "money", version = "0.1" }\nmoney.workspace = true\n',
    });
    expect(dependents(g, 'crates/money/src/lib.rs')).toContain('crates/strings/Cargo.toml');
  });

  it('counts a build script, and the files it reads, as crate inputs', () => {
    const base = { ...SHOP_RUST, 'crates/strings/proto/api.proto': 'syntax = "proto3";\n', 'crates/strings/data/words.txt': 'a\n' };
    const listed = graphOf({
      ...base,
      'crates/strings/build.rs':
        'fn main() {\n    println!("cargo::rerun-if-changed=data/words.txt");\n    prost_build::compile_protos(&["proto/api.proto"], &["proto"]).unwrap();\n}\n',
    });
    expect(dependents(listed, 'crates/strings/build.rs')).toEqual(['crates/strings/Cargo.toml']);
    expect(dependents(listed, 'crates/strings/data/words.txt')).toEqual(['crates/strings/Cargo.toml']);
    expect(dependents(listed, 'crates/strings/proto/api.proto')).toEqual(['crates/strings/Cargo.toml']);

    // Without rerun-if-changed, cargo reruns the script on any package change: every file counts but docs.
    const unlisted = graphOf({ ...base, 'crates/strings/build.rs': 'fn main() {}\n', 'crates/strings/NOTES.md': 'x\n' });
    expect(dependents(unlisted, 'crates/strings/data/words.txt')).toEqual(['crates/strings/Cargo.toml']);
    expect(dependents(unlisted, 'crates/strings/NOTES.md')).toEqual([]);
  });

  it('records an include of a removed file as unresolved', () => {
    const files = { ...SHOP_RUST };
    delete (files as Record<string, string>)['crates/money/README.md'];
    const g = graphOf(files);
    expect([...importersOfMissing(g, ['crates/money/README.md'])]).toEqual(['crates/money/src/lib.rs']);
  });
});

describe('recognize: cargo', () => {
  const at = (dir: string, ...p: string[]) => path.join(dir, ...p);
  const narrow = (argv: string[], cwd: string, files: string[]) => {
    const c = recognize(argv, cwd);
    return c?.kind === 'tests' ? c.narrow(files) : c;
  };

  it('narrows cargo test to -p per crate and --test per integration target', () => {
    const ws = workspace();
    const lib = ['crates/money/Cargo.toml', 'crates/cart/Cargo.toml'];
    expect(narrow(['cargo', 'test', '--workspace', '--locked'], ws, lib)).toEqual([
      'cargo',
      'test',
      '--locked',
      '-p',
      'cart',
      '-p',
      'money',
    ]);
    expect(
      narrow(['cargo', '+nightly', 'test', '--all-features'], ws, ['crates/money/tests/format.rs', 'crates/money/tests/arith.rs']),
    ).toEqual(['cargo', '+nightly', 'test', '--all-features', '-p', 'money', '--test', 'arith', '--test', 'format']);
    // `--test` would apply to every listed package, so one whole crate makes every crate whole.
    expect(narrow(['cargo', 'test'], ws, ['crates/money/tests/arith.rs', 'crates/cart/Cargo.toml'])).toEqual([
      'cargo',
      'test',
      '-p',
      'cart',
      '-p',
      'money',
    ]);
    // Target kinds select more than --test would: crates run whole.
    expect(narrow(['cargo', 'test', '--all-targets'], ws, ['crates/money/tests/arith.rs'])).toEqual([
      'cargo',
      'test',
      '--all-targets',
      '-p',
      'money',
    ]);
    expect(narrow(['cargo', 'test', '--', '--nocapture'], ws, ['crates/money/tests/arith.rs'])).toEqual([
      'cargo',
      'test',
      '-p',
      'money',
      '--test',
      'arith',
      '--',
      '--nocapture',
    ]);
  });

  it('narrows cargo nextest, passing a run that selects no tests', () => {
    const ws = workspace();
    expect(narrow(['cargo', 'nextest', 'run', '--workspace', '-P', 'ci'], ws, ['crates/strings/Cargo.toml'])).toEqual([
      'cargo',
      'nextest',
      'run',
      '-P',
      'ci',
      '--no-tests=pass',
      '-p',
      'strings',
    ]);
    expect(narrow(['cargo', 'nextest', 'run', '--no-tests', 'warn'], ws, ['crates/strings/Cargo.toml'])).toEqual([
      'cargo',
      'nextest',
      'run',
      '--no-tests',
      'warn',
      '-p',
      'strings',
    ]);
  });

  it('follows cargo package selection: default members, a member directory, --exclude', () => {
    const ws = workspace({ ...SHOP_RUST, 'Cargo.toml': '[workspace]\nmembers = ["crates/*"]\ndefault-members = ["crates/money"]\n' });
    const owns = (argv: string[], cwd: string, file: string) => {
      const c = recognize(argv, cwd);
      return c?.kind === 'tests' && c.owns!(path.relative(cwd, at(ws, file)));
    };
    expect(owns(['cargo', 'test'], ws, 'crates/money/Cargo.toml')).toBe(true);
    expect(owns(['cargo', 'test'], ws, 'crates/cart/Cargo.toml')).toBe(false);
    expect(owns(['cargo', 'test', '--workspace'], ws, 'crates/cart/Cargo.toml')).toBe(true);
    expect(owns(['cargo', 'test', '--workspace', '--exclude', 'cart'], ws, 'crates/cart/Cargo.toml')).toBe(false);
    expect(owns(['cargo', 'test'], at(ws, 'crates/cart'), 'crates/cart/Cargo.toml')).toBe(true);
    expect(owns(['cargo', 'test'], at(ws, 'crates/cart'), 'crates/money/Cargo.toml')).toBe(false);
  });

  it('leaves commands that already select tests, or reach beyond the cwd, alone', () => {
    const ws = workspace();
    for (const argv of [
      ['cargo', 'test', 'adds'], // a name filter
      ['cargo', 'test', '--', 'adds'],
      ['cargo', 'test', '-p', 'money'],
      ['cargo', 'test', '--lib'],
      ['cargo', 'test', '--test', 'arith'],
      ['cargo', 'test', '--exclude', 'cart'], // --exclude needs --workspace
      ['cargo', 'test', '--some-new-flag'],
      ['cargo', 'bench'],
    ]) {
      expect(recognize(argv, ws), argv.join(' ')).toBeNull();
    }
    expect(recognize(['cargo', 'test', '--workspace'], at(ws, 'crates/cart'))).toBeNull(); // tests crates outside the cwd
    expect(recognize(['cargo', 'test'], tempDir())).toBeNull(); // no Cargo.toml
  });

  it('treats clippy, check and fmt as whole-workspace checks', () => {
    for (const sub of ['clippy', 'check', 'fmt']) {
      expect(recognize(['cargo', sub, '--all-targets'], tempDir())).toEqual({ kind: 'check', tool: `cargo ${sub}`, ecosystem: 'rust' });
    }
  });
});

describe('cargoScope', () => {
  it('returns null for a virtual manifest run from a member directory with --workspace', () => {
    const ws = workspace();
    expect(cargoScope(path.join(ws, 'crates/money'), true, [])).toBeNull();
    expect(cargoScope(ws, false, [])).not.toBeNull();
  });
});
