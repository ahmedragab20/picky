//! Job-level skip: a setup command (`npm ci`, `composer install`, ...) is skipped when
//! every other command in the GitHub Actions job is a marked command picky skips.
//! Anything picky can't account for keeps the setup.

use std::sync::LazyLock;

use crate::Env;
use crate::config::{Mode, js_string};
use crate::decide::Decision;
use crate::hook::scan_script;
use crate::js::trim;
use crate::jsre::{JsRegex, from_units, units};
use crate::npath;
use crate::plan::{Outcome, plan_command};
use crate::sh::{split_assignments, tokenize};
use crate::yaml::{NodeKind, Value, parse_document};

/// Recognized install commands: they only prepare the job, so they're safe to drop when it does nothing.
pub fn is_setup_command(words: Option<&[String]>) -> bool {
    let Some(words) = words else { return false };
    let rest = split_assignments(words).rest;
    let tool = rest.first().map(String::as_str);
    let sub = rest.get(1).map(String::as_str);
    let args = rest.get(2..).unwrap_or(&[]);
    let flags_only = args.iter().all(|a| a.starts_with('-'));
    let sub_in = |list: &[&str]| sub.is_some_and(|s| list.contains(&s));
    match tool {
        Some("npm") => sub_in(&["ci", "clean-install", "install", "i"]) && flags_only,
        Some("pnpm" | "bun" | "composer") => sub_in(&["install", "i"]) && flags_only,
        Some("yarn") => sub.is_none_or(|s| s.starts_with('-') || (s == "install" && flags_only)),
        Some("npx") => sub == Some("playwright") && matches!(args.first().map(String::as_str), Some("install" | "install-deps")),
        // `cargo install`/`binstall` add tools (cargo-nextest), not project dependencies.
        Some("cargo") => (sub == Some("fetch") && flags_only) || sub == Some("install") || sub == Some("binstall"),
        Some("rustup") => sub_in(&["toolchain", "target", "component", "install", "update", "default", "show"]),
        _ => false,
    }
}

/// Actions that only fetch code, set up tools, or move caches and artifacts. Others keep the setup.
static PASSIVE_ACTIONS: LazyLock<Vec<JsRegex>> = LazyLock::new(|| {
    [
        r"^actions\/(checkout|setup-[\w-]+|cache(\/(restore|save))?|upload-artifact|download-artifact)@",
        r"^(shivammathur\/setup-php|pnpm\/action-setup|oven-sh\/setup-bun)@",
        r"^(dtolnay\/rust-toolchain|actions-rust-lang\/setup-rust-toolchain|actions-rs\/toolchain|Swatinem\/rust-cache|taiki-e\/install-action)@",
        r"^gradle\/(actions\/(setup-gradle|wrapper-validation)|gradle-build-action|wrapper-validation-action)@",
        r"^\.\/\.github\/picky\/?$", // the setup action `picky init` vendors
    ]
    .iter()
    .map(|s| JsRegex::new(s, ""))
    .collect()
});
static WORKFLOW_REF: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^[^/]+\/[^/]+\/(.+?)@", ""));
static PICKY_SHELL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^picky(\s|$)", ""));
static BASH_SHELL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^bash(\s|$)", ""));
static WINDOWS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"windows", "i"));
static MARKER_COMMENT: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*picky\s*$", ""));

#[derive(Clone, Debug, PartialEq)]
pub struct JobPlan {
    pub skip: bool,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub run: Option<String>,
    pub uses: Option<String>,
    /// `run: cmd  # picky` on one line: YAML drops the comment, so the marker never reaches the script.
    pub marked_in_yaml: bool,
    /// `shell: picky {0}`: picky runs the whole step, every command in it marked.
    pub picky_shell: bool,
    /// Bash with BASH_ENV, or the picky shell, so picky sees the step.
    pub hooked: bool,
    /// Working directory relative to the workspace, or None when it's an expression.
    pub dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub workflow: String,
    pub steps: Vec<Step>,
}

/// `value[key]` on a JavaScript value from `doc.toJS()`: own properties and array indices.
pub(crate) fn prop<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    match value? {
        Value::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        Value::Array(items) => {
            let n: u32 = key.parse().ok()?;
            (n.to_string() == key && n != u32::MAX).then(|| items.get(n as usize)).flatten()
        }
        _ => None,
    }
}

/// `value.defaults?.run`
pub(crate) fn run_defaults(v: Option<&Value>) -> Option<&Value> {
    prop(prop(v, "defaults"), "run")
}

pub(crate) fn nullish(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null))
}

/// `a ?? b ?? c`: the first value that isn't null or undefined, else the last.
pub(crate) fn coalesce<'a>(values: &[Option<&'a Value>]) -> Option<&'a Value> {
    values.iter().find(|v| !nullish(**v)).copied().unwrap_or(*values.last().expect("non-empty"))
}

pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => *n != 0.0 && !n.is_nan(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `String(value)`, with `undefined` for a missing value.
pub(crate) fn string_of(v: Option<&Value>) -> String {
    v.map_or("undefined".to_string(), js_string)
}

fn test(re: &JsRegex, s: &str) -> bool {
    re.test(&units(s))
}

/// The current job's steps, read from the workflow file. Ok(Err(reason)) when it can't;
/// Err when reading the workflow throws (an unresolvable alias).
pub fn load_job(root: &str, env: &Env) -> Result<Result<Job, String>, String> {
    let workflow_ref = env.get("GITHUB_WORKFLOW_REF").map(String::as_str).unwrap_or("");
    let u = units(workflow_ref);
    let file = WORKFLOW_REF.exec(&u).map(|m| from_units(m.str(1)).into_owned());
    let job_id = env.get("GITHUB_JOB").filter(|j| !j.is_empty());
    let (Some(file), Some(job_id)) = (file, job_id) else { return Ok(Err("not in GitHub Actions".into())) };
    let Ok(bytes) = std::fs::read(npath::join(&[root, &file])) else { return Ok(Err(format!("can't read {file}"))) };
    let doc = parse_document(&String::from_utf8_lossy(&bytes));
    let workflow = doc.to_js()?;
    let workflow = Some(&workflow);
    let job = prop(prop(workflow, "jobs"), job_id);
    let nodes = doc.get_in(&["jobs", job_id, "steps"]);
    let items = match nodes.map(|n| &n.kind) {
        Some(NodeKind::Seq { items, .. }) if truthy(job) => items,
        _ => return Ok(Err(format!("no job \"{job_id}\" with steps in {file}"))),
    };

    let js_steps = prop(job, "steps");
    let mut steps = Vec::new();
    for (i, node) in items.iter().enumerate() {
        if nullish(js_steps) {
            let what = if js_steps.is_none() { "undefined" } else { "null" };
            return Err(format!("Cannot read properties of {what} (reading '{i}')"));
        }
        let empty = Value::Object(Vec::new());
        let step = Some(coalesce(&[prop(js_steps, &i.to_string()), Some(&empty)]).expect("not nullish"));
        let run_node = match &node.kind {
            NodeKind::Map { .. } => node.get("run"),
            _ => None,
        };
        let shell = coalesce(&[prop(step, "shell"), prop(run_defaults(job), "shell"), prop(run_defaults(workflow), "shell")]);
        let picky_shell = matches!(shell, Some(Value::String(s)) if test(&PICKY_SHELL, s));
        // In a container the hook path (a host path) doesn't exist, and on Windows there's no bash by default.
        let hookable = !truthy(prop(job, "container")) && !test(&WINDOWS, &string_of(prop(job, "runs-on")));
        let dir_value = coalesce(&[
            prop(step, "working-directory"),
            prop(run_defaults(job), "working-directory"),
            prop(run_defaults(workflow), "working-directory"),
        ]);
        let dir = if nullish(dir_value) { ".".to_string() } else { string_of(dir_value) };
        let marked_in_yaml = match run_node {
            Some(n) if matches!(n.kind, NodeKind::Scalar { .. }) => test(&MARKER_COMMENT, n.comment.as_deref().unwrap_or("")),
            _ => false,
        };
        steps.push(Step {
            run: match prop(step, "run") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            },
            uses: prop(step, "uses").map(js_string),
            marked_in_yaml,
            picky_shell,
            hooked: picky_shell || (hookable && (shell.is_none() || test(&BASH_SHELL, &string_of(shell)))),
            dir: if dir.contains("${{") { None } else { Some(dir) },
        });
    }
    Ok(Ok(Job { workflow: file, steps }))
}

/// The workflow step running `script`; a reusable or composite workflow can share the job id, so it must match.
pub fn current_step<'a>(job: &'a Job, script: &str) -> Option<&'a Step> {
    let text = script.replace("\r\n", "\n");
    let text = trim(&text);
    job.steps.iter().find(|s| s.run.as_deref().is_some_and(|r| trim(r) == text))
}

struct Marked {
    words: Vec<String>,
    cwd: String,
    text: String,
}

/// Decides one setup line of the current step. `decide` is only called when the job could be skipped.
pub fn plan_setup(script: &str, root: &str, env: &Env, decide: impl FnOnce() -> Result<Decision, String>) -> Result<JobPlan, String> {
    let keep = |note: String| Ok(JobPlan { skip: false, note });
    let job = match load_job(root, env)? {
        Ok(job) => job,
        Err(reason) => return keep(format!("setup runs (job-level skip: {reason})")),
    };
    if current_step(&job, script).is_none() {
        let id = env.get("GITHUB_JOB").map_or("undefined", String::as_str);
        return keep(format!("setup runs (this step isn't in job \"{id}\" of {})", job.workflow));
    }

    // The git root is a real path; the workspace may reach it through a symlink.
    let workspace = realpath(env.get("GITHUB_WORKSPACE").map_or(root, String::as_str));
    let mut marked: Vec<Marked> = Vec::new();
    for step in &job.steps {
        if let Some(uses) = &step.uses {
            if !PASSIVE_ACTIONS.iter().any(|re| test(re, uses)) {
                return keep(format!("setup runs (step uses {uses})"));
            }
            continue;
        }
        let Some(run) = &step.run else { continue };
        for line in scan_script(run, step.marked_in_yaml || step.picky_shell) {
            let words = if line.simple { tokenize(&line.text) } else { None };
            if !line.marked && is_setup_command(words.as_deref()) {
                continue;
            }
            let label = trim(&line.text).to_string();
            let words = match words {
                Some(w) if line.marked && !w.is_empty() => w,
                _ => return keep(format!("setup runs (the job also runs `{label}`)")),
            };
            let Some(dir) = step.dir.as_deref().filter(|_| step.hooked) else {
                return keep(format!("setup runs (picky can't decide `{label}` here)"));
            };
            marked.push(Marked { words, cwd: realpath(&npath::resolve(&[&workspace, dir])), text: label });
        }
    }
    if marked.is_empty() {
        return keep("setup runs (no marked commands in this job)".into());
    }

    let decision = decide()?;
    for m in &marked {
        let outcome = plan_command(&m.words, &m.cwd, &realpath(root), &decision)?;
        if !matches!(outcome, Outcome::Skip { .. }) {
            let shadow = decision.mode == Mode::Shadow && outcome.note().contains("would skip");
            if !shadow {
                return keep(format!("setup runs (`{}`: {})", m.text, outcome.note()));
            }
        }
    }
    if decision.mode == Mode::Shadow {
        return keep(format!("shadow mode: would skip setup (all {} marked commands skip); running", marked.len()));
    }
    Ok(JobPlan { skip: true, note: format!("skip: all {} marked commands in this job skip", marked.len()) })
}

fn realpath(p: &str) -> String {
    npath::realpath(p).unwrap_or_else(|| p.to_string())
}
