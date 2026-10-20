mod common;

use common::*;
use picky::git::{changed_files, resolve_range};

#[test]
fn diffs_a_pull_request_merge_commit_against_its_first_parent_even_with_a_stale_base_sha() {
    // feature changes math.ts; main later gains its own commit; GitHub merges feature into the new main.
    let root = repo(&shop(), &files(&[("src/math.ts", "export const add = (a: number, b: number) => b + a;\n")]));
    let rev = |r: &str| git(&root, &["rev-parse", r]).trim().to_string();
    let stale_base = rev("main");
    let pr_head = rev("feature");
    git(&root, &["checkout", "-q", "main"]);
    write_files(&root, &files(&[("README.md", "# moved on\n")]));
    git(&root, &["commit", "-qam", "main moves on"]);
    git(&root, &["checkout", "-q", "--detach"]);
    git(&root, &["merge", "-q", "--no-ff", "-m", "merge", "feature"]);

    let event = format!("{}/.git/event.json", &*root);
    let body = format!(r#"{{"pull_request":{{"base":{{"sha":"{stale_base}"}},"head":{{"sha":"{pr_head}"}}}}}}"#);
    std::fs::write(&event, body).unwrap();
    let range = resolve_range(&root, &clean_env(&[("GITHUB_EVENT_PATH", &event)])).unwrap();

    assert_eq!(range.via, "pull request merge commit (first parent)");
    let paths: Vec<String> = changed_files(&root, &range).unwrap().into_iter().map(|c| c.path).collect();
    assert_eq!(paths, ["src/math.ts"]);
}
