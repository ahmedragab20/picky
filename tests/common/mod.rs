//! Fixtures shared by the integration tests: temp dirs, git repos and sample projects.
#![allow(dead_code)]

use std::process::Command;

use indexmap::IndexMap;
use picky::Env;

/// A temporary directory, removed when dropped; derefs to its path.
pub struct Dir {
    _tmp: tempfile::TempDir,
    pub path: String,
}

impl std::ops::Deref for Dir {
    type Target = str;
    fn deref(&self) -> &str {
        &self.path
    }
}

pub fn temp_dir() -> Dir {
    let tmp = tempfile::Builder::new().prefix("picky-test-").tempdir().expect("temp dir");
    let path = tmp.path().to_string_lossy().into_owned();
    Dir { _tmp: tmp, path }
}

/// Files to write: None removes the file.
pub type Files = IndexMap<String, Option<String>>;

pub fn files(entries: &[(&str, &str)]) -> Files {
    entries.iter().map(|(k, v)| (k.to_string(), Some(v.to_string()))).collect()
}

pub fn removed(file: &str) -> Files {
    Files::from([(file.to_string(), None)])
}

/// `{ ...a, ...b }`: later entries win, keeping the first position.
pub fn cat(parts: &[&Files]) -> Files {
    let mut out = Files::new();
    for part in parts {
        for (k, v) in part.iter() {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

pub fn write_files(dir: &str, files: &Files) {
    for (file, content) in files {
        let full = std::path::Path::new(dir).join(file);
        match content {
            None => std::fs::remove_file(&full).expect("remove"),
            Some(text) => {
                std::fs::create_dir_all(full.parent().unwrap()).expect("mkdir");
                std::fs::write(&full, text).expect("write");
            }
        }
    }
}

pub fn sh(dir: &str, cmd: &str, args: &[&str]) -> String {
    let out = Command::new(cmd).args(args).current_dir(dir).output().expect("spawn");
    assert!(out.status.success(), "{cmd} {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf-8")
}

pub fn git(dir: &str, args: &[&str]) -> String {
    let mut all = vec!["-c", "user.name=t", "-c", "user.email=t@t"];
    all.extend_from_slice(args);
    sh(dir, "git", &all)
}

/// A git repo on `main` with `base` committed, then a `feature` branch with `change` committed.
pub fn repo(base: &Files, change: &Files) -> Dir {
    let dir = temp_dir();
    git(&dir, &["init", "-q", "-b", "main"]);
    write_files(&dir, base);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "base"]);
    git(&dir, &["checkout", "-qb", "feature"]);
    write_files(&dir, change);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "change", "--allow-empty"]);
    dir
}

/// Environment for decide(): no CI variables leak in from the machine running the tests.
pub fn clean_env(extra: &[(&str, &str)]) -> Env {
    let mut env = Env::new();
    for key in ["PATH", "HOME"] {
        if let Ok(v) = std::env::var(key) {
            env.insert(key.into(), v);
        }
    }
    for (k, v) in extra {
        env.insert(k.to_string(), v.to_string());
    }
    env
}

/// The picky binary under test.
pub const PICKY: &str = env!("CARGO_BIN_EXE_picky");

/// The repository root.
pub const HOME: &str = env!("CARGO_MANIFEST_DIR");

pub fn graph_only() -> Files {
    files(&[(".picky.yml", "mode: enforce\nmodel: none\n")])
}

pub fn shop() -> Files {
    files(&[
        ("package.json", r#"{"name":"shop","type":"module","scripts":{"test":"node --test"}}"#),
        ("src/math.ts", "export const add = (a: number, b: number) => a + b;\n"),
        ("src/cart.ts", "import { add } from './math.ts';\nexport const total = (xs: number[]) => xs.reduce(add, 0);\n"),
        ("src/strings.ts", "export const shout = (s: string) => s.toUpperCase();\n"),
        ("test/math.test.ts", "import { add } from '../src/math.ts';\ntest('adds', () => {});\n"),
        ("test/cart.test.ts", "import { total } from '../src/cart.ts';\ntest('totals', () => {});\n"),
        ("test/strings.test.ts", "import { shout } from '../src/strings.ts';\ndescribe('shout', () => {});\n"),
        ("README.md", "# shop\n"),
    ])
}

/// A small PHP app: PSR-4 classes, PHPUnit tests and a Pest suite.
pub fn shop_php() -> Files {
    files(&[
        ("composer.json", r#"{"autoload":{"psr-4":{"App\\":"app/"}},"autoload-dev":{"psr-4":{"Tests\\":"tests/"}}}"#),
        ("composer.lock", r#"{"packages":[],"packages-dev":[{"name":"phpunit/phpunit","version":"11.5.0"}]}"#),
        ("app/Money.php", "<?php\nnamespace App;\n\nfinal class Money\n{\n    public function __construct(public int $cents) {}\n}\n"),
        ("app/Cart.php", "<?php\nnamespace App;\n\nclass Cart\n{\n    /** @var Money[] */\n    private array $items = [];\n}\n"),
        ("app/Models/User.php", "<?php\nnamespace App\\Models;\n\nclass User {}\n"),
        ("app/helpers.php", "<?php\n\nfunction money_format_cents(int $c): string { return (string) $c; }\n"),
        ("tests/TestCase.php", "<?php\nnamespace Tests;\n\nabstract class TestCase extends \\PHPUnit\\Framework\\TestCase {}\n"),
        (
            "tests/Unit/CartTest.php",
            "<?php\nnamespace Tests\\Unit;\n\nuse App\\Cart;\nuse Tests\\TestCase;\n\nclass CartTest extends TestCase\n{\n    public function testEmpty(): void {}\n}\n",
        ),
        (
            "tests/Unit/UserTest.php",
            "<?php\nnamespace Tests\\Unit;\n\nuse App\\Models\\{User};\nuse Tests\\TestCase;\n\nclass UserTest extends TestCase {}\n",
        ),
        (
            "tests/Unit/FormatTest.php",
            "<?php\nnamespace Tests\\Unit;\n\nrequire_once __DIR__ . '/../../app/helpers.php';\n\nclass FormatTest extends \\Tests\\TestCase {}\n",
        ),
        ("tests/Feature/Pest.php", "<?php\nuses(Tests\\TestCase::class)->in('.');\n"),
        ("tests/Feature/HomeTest.php", "<?php\ntest('home', function () { expect(true)->toBeTrue(); });\n"),
    ])
}

/// A Cargo workspace: `cart` depends on `money`, `strings` stands alone; `money` has two integration tests.
pub fn shop_rust() -> Files {
    files(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n"),
        ("Cargo.lock", "# This file is automatically @generated by Cargo.\nversion = 4\n"),
        ("crates/money/Cargo.toml", "[package]\nname = \"money\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("crates/money/README.md", "# money\n\n```\nassert_eq!(money::add(1, 2), 3);\n```\n"),
        (
            "crates/money/src/lib.rs",
            "#![doc = include_str!(\"../README.md\")]\npub mod fmt;\n\npub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(1, 2), 3);\n    }\n}\n",
        ),
        ("crates/money/src/fmt.rs", "pub fn cents(c: i64) -> String {\n    format!(\"{c}c\")\n}\n"),
        ("crates/money/tests/arith.rs", "mod common;\n\n#[test]\nfn adds() {\n    assert_eq!(money::add(1, 2), 3);\n}\n"),
        ("crates/money/tests/format.rs", "mod common;\n\n#[test]\nfn formats() {\n    assert_eq!(money::fmt::cents(3), \"3c\");\n}\n"),
        ("crates/money/tests/common/mod.rs", "pub fn setup() {}\n"),
        (
            "crates/cart/Cargo.toml",
            "[package]\nname = \"cart\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nmoney = { path = \"../money\" }\n",
        ),
        (
            "crates/cart/src/lib.rs",
            "pub fn total(xs: &[i64]) -> i64 {\n    xs.iter().fold(0, |a, b| money::add(a, *b))\n}\n\n#[test]\nfn totals() {\n    assert_eq!(total(&[1, 2]), 3);\n}\n",
        ),
        ("crates/strings/Cargo.toml", "[package]\nname = \"strings\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        (
            "crates/strings/src/lib.rs",
            "pub fn shout(s: &str) -> String {\n    s.to_uppercase()\n}\n\n#[test]\nfn shouts() {\n    assert_eq!(shout(\"a\"), \"A\");\n}\n",
        ),
    ])
}

/// A two-project Gradle build in Kotlin: `app` depends on `core`; JUnit tests in both, one in Java.
pub fn shop_kotlin() -> Files {
    files(&[
        ("settings.gradle.kts", "rootProject.name = \"shop\"\ninclude(\"core\", \"app\")\n"),
        ("build.gradle.kts", "plugins {\n    kotlin(\"jvm\") version \"2.4.10\" apply false\n}\n"),
        ("core/build.gradle.kts", "plugins {\n    kotlin(\"jvm\")\n}\n"),
        ("app/build.gradle.kts", "plugins {\n    kotlin(\"jvm\")\n}\n\ndependencies {\n    implementation(project(\":core\"))\n}\n"),
        (
            "core/src/main/kotlin/shop/Money.kt",
            "package shop\n\ndata class Money(val cents: Long) {\n    operator fun plus(o: Money) = Money(cents + o.cents)\n}\n",
        ),
        (
            "core/src/main/kotlin/shop/Cart.kt",
            "package shop\n\nclass Cart(private val items: List<Money>) {\n    fun total() = items.fold(Money(0)) { a, b -> a + b }\n}\n",
        ),
        ("core/src/main/kotlin/shop/text/Strings.kt", "package shop.text\n\nfun shout(s: String) = s.uppercase()\n"),
        (
            "core/src/test/kotlin/shop/MoneyTest.kt",
            "package shop\n\nimport kotlin.test.Test\nimport kotlin.test.assertEquals\n\nclass MoneyTest {\n    @Test\n    fun adds() = assertEquals(Money(3), Money(1) + Money(2))\n}\n",
        ),
        (
            "core/src/test/kotlin/shop/CartTest.kt",
            "package shop\n\nimport kotlin.test.Test\n\nclass CartTest {\n    @Test\n    fun `totals items`() {\n        Cart(listOf()).total()\n    }\n}\n",
        ),
        ("core/src/test/resources/cart.json", "{\"items\": []}\n"),
        ("app/src/main/kotlin/app/Main.kt", "package app\n\nimport shop.text.shout\n\nfun main() = println(shout(\"hi\"))\n"),
        (
            "app/src/test/java/app/ShoutTest.java",
            "package app;\n\nimport org.junit.jupiter.api.Test;\nimport shop.text.StringsKt;\n\nclass ShoutTest {\n    @Test\n    void shouts() {\n        StringsKt.shout(\"a\");\n    }\n}\n",
        ),
    ])
}

pub fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Sorted copy, for comparing sets.
pub fn sorted<I: IntoIterator<Item = S>, S: Into<String>>(items: I) -> Vec<String> {
    let mut v: Vec<String> = items.into_iter().map(Into::into).collect();
    v.sort();
    v
}
