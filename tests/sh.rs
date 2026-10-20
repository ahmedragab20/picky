use picky::sh::{quote, quote_argv, split_assignments, tokenize};

#[test]
fn tokenize_splits_words_and_handles_quotes_and_escapes() {
    assert_eq!(
        tokenize(r#"npx vitest run --reporter='dot' "a b" c\ d"#).unwrap(),
        ["npx", "vitest", "run", "--reporter=dot", "a b", "c d"]
    );
}

#[test]
fn tokenize_drops_a_trailing_comment_such_as_the_picky_marker() {
    assert_eq!(tokenize("npm test   # picky").unwrap(), ["npm", "test"]);
}

#[test]
fn tokenize_refuses_shell_syntax_it_cannot_rewrite() {
    for (what, line) in [
        ("pipes", "npm test | tee out"),
        ("and-lists", "npm ci && npm test"),
        ("redirects", "npm test > log"),
        ("substitution", "vitest $(cat files)"),
        ("variables in double quotes", r#"vitest "$FILES""#),
        ("globs", "vitest src/*.test.ts"),
        ("line continuations", "vitest run \\"),
        ("unterminated quotes", "vitest 'oops"),
    ] {
        assert_eq!(tokenize(line), None, "{what}");
    }
}

#[test]
fn quote_leaves_plain_words_alone_and_single_quotes_the_rest() {
    assert_eq!(quote("test/a.test.ts"), "test/a.test.ts");
    assert_eq!(quote("it's here"), r"'it'\''s here'");
}

#[test]
fn quoting_keeps_leading_assignments_as_assignments() {
    assert_eq!(quote_argv(&["CI=a b", "npx", "vitest"]), "CI='a b' npx vitest");
    let split = split_assignments(&["A=1", "B=2", "jest", "X=3"]);
    assert_eq!(split.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect::<Vec<_>>(), [("A", "1"), ("B", "2")]);
    assert_eq!(split.rest, ["jest", "X=3"]);
}
