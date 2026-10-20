use std::path::Path;
use std::process::{Command, Stdio};

use indexmap::IndexMap;

use crate::Env;
use crate::js;

/// Runs git and returns its stdout. A failure carries the message Node's `execFileSync` throws.
pub fn git(cwd: &str, args: &[&str]) -> Result<String, String> {
    // A container job runs as root over a workspace the runner user owns, which git refuses as
    // "dubious ownership". picky only reads the repo the job already runs code from.
    let mut argv: Vec<&str> = Vec::new();
    if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true") {
        argv.extend(["-c", "safe.directory=*"]);
    }
    argv.extend(args);
    let out = Command::new("git")
        .args(&argv)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("spawnSync git {}", spawn_error_code(&e)))?;
    if !out.status.success() {
        let mut msg = format!("Command failed: git {}", argv.join(" "));
        if !out.stderr.is_empty() {
            msg.push('\n');
            msg.push_str(&String::from_utf8_lossy(&out.stderr));
        }
        return Err(msg);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The `code` Node puts in a spawn error message (`spawnSync git ENOENT`).
pub fn spawn_error_code(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT".into(),
        std::io::ErrorKind::PermissionDenied => "EACCES".into(),
        _ => e.to_string(),
    }
}

fn try_git(cwd: &str, args: &[&str]) -> Option<String> {
    git(cwd, args).ok().map(|s| js::trim(&s).to_string())
}

pub fn repo_root(cwd: &str) -> Result<String, String> {
    Ok(js::trim(&git(cwd, &["rev-parse", "--show-toplevel"])?).to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub base: String,
    pub head: String,
    /// How the base was found, for the summary.
    pub via: String,
}

/// Finds the commit to diff against. Prefers the merge base so commits on the
/// target branch don't count as changes. When only the base tip is reachable, a
/// two-tree diff over-selects, which is the safe direction.
pub fn resolve_range(root: &str, env: &Env) -> Option<Range> {
    let head = try_git(root, &["rev-parse", "HEAD"]).filter(|h| !h.is_empty())?;

    if let Some(explicit) = env.get("PICKY_BASE").filter(|b| !b.is_empty()) {
        let base = try_git(root, &["merge-base", explicit, &head])
            .or_else(|| try_git(root, &["rev-parse", explicit]))
            .filter(|b| !b.is_empty())?;
        return Some(Range { base, head, via: format!("PICKY_BASE={explicit}") });
    }

    if let Some(merge) = github_merge_parent(root, &head, env) {
        return Some(Range { base: merge, head, via: "pull request merge commit (first parent)".into() });
    }

    if let Some(pr_base) = github_pull_request_base(env) {
        if let Some(mb) = try_git(root, &["merge-base", &pr_base, &head]) {
            return Some(Range { base: mb, head, via: "pull request merge base".into() });
        }
        if try_git(root, &["cat-file", "-e", &format!("{pr_base}^{{commit}}")]).is_none() {
            try_git(root, &["fetch", "--no-tags", "--depth=1", "origin", &pr_base]);
        }
        let tip = try_git(root, &["rev-parse", &format!("{pr_base}^{{commit}}")]).filter(|t| !t.is_empty())?;
        return Some(Range { base: tip, head, via: "pull request base tip (shallow)".into() });
    }

    for r in ["origin/HEAD", "origin/main", "main", "origin/master", "master"] {
        if let Some(mb) = try_git(root, &["merge-base", r, &head])
            && !mb.is_empty()
            && mb != head
        {
            return Some(Range { base: mb, head, via: format!("merge base with {r}") });
        }
    }
    None
}

/// `JSON.parse(event).pull_request`, when it's an object.
fn pull_request(env: &Env) -> Option<serde_json::Value> {
    let file = env.get("GITHUB_EVENT_PATH").filter(|p| !p.is_empty())?;
    let text = std::fs::read(file).ok()?;
    let event: serde_json::Value = crate::json::parse(&String::from_utf8_lossy(&text)).ok()?;
    event.get("pull_request").filter(|v| !v.is_null()).cloned()
}

/// `pr?.<side>?.sha`, when it's a string. (A number would be stringified by the git call; a
/// pull request event never holds one.)
fn sha(pr: &serde_json::Value, side: &str) -> Option<String> {
    pr.get(side)?.get("sha")?.as_str().map(str::to_string)
}

fn github_pull_request_base(env: &Env) -> Option<String> {
    sha(&pull_request(env)?, "base").filter(|s| !s.is_empty())
}

/// On a pull_request event, the checkout is GitHub's merge of the PR into the current base.
/// Its first parent is that base, so diffing against it gives exactly the PR's changes, even
/// when the event's base sha is stale because the base branch moved on.
fn github_merge_parent(root: &str, head: &str, env: &Env) -> Option<String> {
    let pr_head = sha(&pull_request(env)?, "head").filter(|s| !s.is_empty())?;
    // The raw commit keeps its parent ids even in a shallow clone, where history is cut off.
    let raw = try_git(root, &["cat-file", "-p", head]).unwrap_or_default();
    let parents: Vec<&str> = raw.split('\n').filter_map(|l| l.strip_prefix("parent ")).collect();
    if parents.len() != 2 || parents[1] != pr_head {
        return None;
    }
    let first = format!("{}^{{commit}}", parents[0]);
    if try_git(root, &["cat-file", "-e", &first]).is_none() {
        try_git(root, &["fetch", "--no-tags", "--depth=1", "origin", parents[0]]);
    }
    try_git(root, &["cat-file", "-e", &first]).map(|_| parents[0].to_string())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Change {
    pub status: String,
    pub path: String,
    /// Previous path for renames and copies.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub from: Option<String>,
    pub added: u64,
    pub removed: u64,
}

pub fn changed_files(root: &str, range: &Range) -> Result<Vec<Change>, String> {
    // -z keeps paths unquoted and unambiguous.
    let numstat_out = git(root, &["diff", "--numstat", "-z", "-M", &range.base, &range.head])?;
    let numstat: Vec<&str> = numstat_out.split('\0').collect();
    let mut counts: IndexMap<String, (u64, u64)> = IndexMap::new();
    let mut i = 0;
    while i < numstat.len() {
        if numstat[i].is_empty() {
            i += 1;
            continue;
        }
        let mut fields = numstat[i].split('\t');
        let a = fields.next().unwrap_or("");
        let r = fields.next().unwrap_or("");
        let file = fields.next();
        // Renames print an empty path, then the old and new paths as separate fields.
        let target = if file == Some("") {
            i += 2;
            numstat.get(i).copied()
        } else {
            file
        };
        // `Number(a) || 0`: counts are digits, or `-` for a binary file.
        let n = |s: &str| {
            let v = js::parse_number(s);
            if v.is_finite() && v > 0.0 { v as u64 } else { 0 }
        };
        // `counts.set(undefined, ...)` keys a missing path as "undefined" in JavaScript; no
        // path is ever looked up by that name, so it's dropped here.
        if let Some(t) = target {
            counts.insert(t.to_string(), (n(a), n(r)));
        }
        i += 1;
    }

    let status_out = git(root, &["diff", "--name-status", "-z", "-M", &range.base, &range.head])?;
    let fields: Vec<&str> = status_out.split('\0').collect();
    let mut changes = Vec::new();
    let mut i = 0;
    while i < fields.len() && !fields[i].is_empty() {
        let status = fields[i].chars().next().map(String::from).unwrap_or_default();
        if status == "R" || status == "C" {
            let from = fields.get(i + 1).copied().unwrap_or("undefined");
            let file = fields.get(i + 2).copied().unwrap_or("undefined");
            let (added, removed) = counts.get(file).copied().unwrap_or((0, 0));
            changes.push(Change { status, path: file.into(), from: Some(from.into()), added, removed });
            i += 3;
        } else {
            let file = fields.get(i + 1).copied().unwrap_or("undefined");
            let (added, removed) = counts.get(file).copied().unwrap_or((0, 0));
            changes.push(Change { status, path: file.into(), from: None, added, removed });
            i += 2;
        }
    }
    Ok(changes)
}

/// Zero-context unified diff for the given files.
pub fn hunks(root: &str, range: &Range, files: &[String]) -> Result<String, String> {
    if files.is_empty() {
        return Ok(String::new());
    }
    let mut args = vec!["diff", "-U0", "-M", "--no-color", &range.base, &range.head, "--"];
    args.extend(files.iter().map(String::as_str));
    git(root, &args)
}

pub fn tracked_files(root: &str) -> Result<Vec<String>, String> {
    Ok(git(root, &["ls-files", "-z"])?.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect())
}

/// True when `p` exists (Node's `existsSync`).
pub fn exists(p: &str) -> bool {
    !p.is_empty() && Path::new(p).exists()
}
