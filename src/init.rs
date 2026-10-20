//! `picky init`: the files picky needs in a repo, and the workflow edits that turn it on.
//! Workflows are edited as text at positions the YAML parser reports, so formatting and
//! comments stay exactly as they were.

use std::sync::LazyLock;

use indexmap::IndexSet;
use sha2::{Digest, Sha256};

use crate::commands::recognize;
use crate::config::js_string;
use crate::hook::scan_script;
use crate::job::{coalesce, is_setup_command, nullish, prop, run_defaults, string_of, truthy};
use crate::js::{sort_strings, trim, trim_end};
use crate::jsre::{JsRegex, units};
use crate::npath;
use crate::sh::tokenize;
use crate::shell::Shell;
use crate::yaml::{Node, NodeKind, Pair, ScalarStyle, Value, parse_document};

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    Binary(Vec<u8>),
}

impl Content {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Content::Text(s) => s.as_bytes(),
            Content::Binary(b) => b,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitFile {
    pub path: String,
    /// Current content, or None for a new file.
    pub before: Option<Vec<u8>>,
    /// New content, or None to remove the file.
    pub after: Option<Content>,
    pub mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitPlan {
    pub files: Vec<InitFile>,
    pub notes: Vec<String>,
}

/// The platforms picky ships a binary for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    LinuxX64,
    LinuxArm64,
    DarwinX64,
    DarwinArm64,
    WindowsX64,
}

impl Platform {
    pub const ALL: [Platform; 5] =
        [Platform::LinuxX64, Platform::LinuxArm64, Platform::DarwinX64, Platform::DarwinArm64, Platform::WindowsX64];

    pub fn as_str(self) -> &'static str {
        match self {
            Platform::LinuxX64 => "linux-x64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::DarwinX64 => "darwin-x64",
            Platform::DarwinArm64 => "darwin-arm64",
            Platform::WindowsX64 => "windows-x64",
        }
    }

    /// The release asset, and the file under `.github/picky/bin`.
    pub fn file_name(self) -> String {
        let exe = if self == Platform::WindowsX64 { ".exe" } else { "" };
        format!("picky-{}{exe}", self.as_str())
    }

    pub fn parse(name: &str) -> Option<Platform> {
        Platform::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

/// Where `picky init` gets the binaries it vendors.
pub trait Binaries {
    /// The binary for `platform`, or None when `current` (the one vendored now) is known to be it.
    fn get(&mut self, platform: Platform, current: Option<&[u8]>) -> Result<Option<Vec<u8>>, String>;
}

/// The POSIX entry point `picky init` vendors: it runs the binary for this machine.
pub const WRAPPER: &str = r#"#!/bin/sh
# Runs the picky binary vendored next to this file, for this machine's OS and CPU. Without
# one, a `shell: picky {0}` step still runs, unchanged.
here="$(dirname "$0")"
case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=darwin ;;
  MINGW* | MSYS* | CYGWIN*) os=windows ;;
  *) os=unknown ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) cpu=x64 ;;
  aarch64 | arm64) cpu=arm64 ;;
  *) cpu=unknown ;;
esac
bin="$here/bin/picky-$os-$cpu"
if [ "$os" = windows ]; then bin="$bin.exe"; fi
if "$bin" --version >/dev/null 2>&1; then exec "$bin" "$@"; fi
echo "picky: no picky binary for $os-$cpu in $here/bin; running the step unchanged" >&2
case "$1" in
  bash) exec bash --noprofile --norc -eo pipefail "$2" ;;
  sh) exec sh -e "$2" ;;
esac
if [ $# -eq 1 ] && [ -f "$1" ]; then exec sh -e "$1"; fi
exit 127
"#;

/// Windows resolves `shell: picky {0}` to this through PATHEXT. Without the binary, the step
/// runs unchanged in the shell it names (pwsh by default), the way GitHub runs it.
pub const WRAPPER_CMD: &str = concat!(
    "@echo off\r\n",
    "rem Runs the picky binary vendored next to this file.\r\n",
    "if not exist \"%~dp0bin\\picky-windows-x64.exe\" goto fallback\r\n",
    "\"%~dp0bin\\picky-windows-x64.exe\" %*\r\n",
    "exit /b %ERRORLEVEL%\r\n",
    ":fallback\r\n",
    "echo picky: no picky binary in %~dp0bin; running the step unchanged 1>&2\r\n",
    "set \"script=%~2\"\r\n",
    "if /i \"%~1\"==\"bash\" goto bash\r\n",
    "if /i \"%~1\"==\"sh\" goto sh\r\n",
    "if /i \"%~1\"==\"powershell\" goto powershell\r\n",
    "if /i \"%~1\"==\"pwsh\" goto pwsh\r\n",
    "if not \"%~2\"==\"\" exit /b 127\r\n",
    "set \"script=%~1\"\r\n",
    "if not exist \"%script%\" exit /b 127\r\n",
    ":pwsh\r\n",
    "copy /y \"%script%\" \"%script%.ps1\" >nul\r\n",
    "pwsh -command \". '%script%.ps1'\"\r\n",
    "exit /b %ERRORLEVEL%\r\n",
    ":powershell\r\n",
    "copy /y \"%script%\" \"%script%.ps1\" >nul\r\n",
    "powershell -command \". '%script%.ps1'\"\r\n",
    "exit /b %ERRORLEVEL%\r\n",
    ":bash\r\n",
    "bash --noprofile --norc -eo pipefail \"%script%\"\r\n",
    "exit /b %ERRORLEVEL%\r\n",
    ":sh\r\n",
    "sh -e \"%script%\"\r\n",
    "exit /b %ERRORLEVEL%\r\n",
);

/// `uses: ./.github/picky`: puts picky on PATH for `shell: picky {0}` steps.
pub const SETUP_ACTION: &str = r#"name: picky setup
description: "Puts picky on PATH, for steps that run with `shell: picky {0}` (Windows, containers, shells other than bash)."
runs:
  using: composite
  steps:
    - if: runner.os == 'Windows'
      shell: pwsh
      run: Add-Content -Path $env:GITHUB_PATH -Value $env:GITHUB_ACTION_PATH
    - if: runner.os != 'Windows'
      shell: sh
      run: echo "$GITHUB_ACTION_PATH" >> "$GITHUB_PATH"
"#;

const CONFIG: &str = "# picky: runs only the tests, lint and type checks a change can affect.
mode: shadow   # decide and log, but run everything. Switch to enforce once the summaries look right.
model: jev     # jev | typesafe/<jev-version> | none (graph only)
# critical: [\"db/**\"]   # a change here, or a test here, always runs
";

/// The step hook `picky init` installs.
pub const HOOK: &str = include_str!("../hook/hook.sh");

const ENV: [(&str, &str); 2] =
    [("BASH_ENV", "${{ github.workspace }}/.github/picky/hook.sh"), ("PICKY_API_KEY", "${{ secrets.PICKY_API_KEY }}")];

/// What the Node version of picky vendored; the binaries replace it.
const LEGACY_BUNDLE: &str = ".github/picky/picky.mjs";

static PICKY_SHELL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^picky(\s|$)", ""));
static BASH_SHELL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^bash(\s|$)", ""));
static WINDOWS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"windows", "i"));
static MARKER_COMMENT: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*picky\s*$", ""));
static SETUP_ACTION_USES: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\.\/\.github\/picky\/?$", ""));
static MACOS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"macos|darwin|osx", "i"));
static MACOS_ARM: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"xlarge$|arm64|aarch64", "i"));
static MACOS_INTEL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"large$|intel|x64|x86|^macos-1[0-3]($|\D)", "i"));
static ARM: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"(^|[-_ ])arm(64)?([-_ ]|$)|aarch64", "i"));
static MATRIX_EXPRESSION: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\$\{\{\s*matrix\.([\w-]+(\.[\w-]+)*)\s*\}\}$", ""));

fn test(re: &JsRegex, s: &str) -> bool {
    re.test(&units(s))
}

pub fn plan_init(root: &str, binaries: &mut dyn Binaries) -> Result<InitPlan, String> {
    let mut files: Vec<InitFile> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let add = |files: &mut Vec<InitFile>, rel: &str, after: Content, mode: Option<u32>| {
        let before = std::fs::read(npath::join(&[root, rel])).ok();
        if before.as_deref() != Some(after.bytes()) {
            files.push(InitFile { path: rel.to_string(), before, after: Some(after), mode });
        }
    };

    if !exists(&npath::join(&[root, ".picky.yml"])) {
        add(&mut files, ".picky.yml", Content::Text(CONFIG.into()), None);
    }
    add(&mut files, ".github/picky/hook.sh", Content::Text(HOOK.into()), None);
    add(&mut files, ".github/picky/picky", Content::Text(WRAPPER.into()), Some(0o755));
    add(&mut files, ".github/picky/picky.cmd", Content::Text(WRAPPER_CMD.into()), None);
    add(&mut files, ".github/picky/action.yml", Content::Text(SETUP_ACTION.into()), None);

    let dir = npath::join(&[root, ".github/workflows"]);
    let mut workflows: Vec<String> = Vec::new();
    if exists(&dir) {
        for entry in std::fs::read_dir(&dir).map_err(|e| node_error(&e, "scandir", &dir))? {
            let name = entry.map_err(|e| node_error(&e, "scandir", &dir))?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".yml") || name.ends_with(".yaml") {
                workflows.push(name);
            }
        }
    }
    sort_strings(&mut workflows);
    if workflows.is_empty() {
        notes.push("no workflows in .github/workflows; add `# picky` to a step and the env lines by hand".into());
    }
    let mut platforms: IndexSet<Platform> = IndexSet::new();
    let mut edited: Vec<(String, String)> = Vec::new();
    for name in &workflows {
        let rel = format!(".github/workflows/{name}");
        let full = npath::join(&[root, &rel]);
        let bytes = std::fs::read(&full).map_err(|e| node_error(&e, "read", &full))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let result = edit_workflow(&text, root)?;
        notes.extend(result.notes.iter().map(|n| format!("{rel}: {n}")));
        platforms.extend(result.platforms);
        if result.text != text {
            edited.push((rel, result.text));
        }
    }

    // One binary for each runner picky's jobs use; binaries vendored earlier stay up to date.
    let bin = npath::join(&[root, ".github/picky/bin"]);
    for platform in Platform::ALL {
        if exists(&npath::join(&[&bin, &platform.file_name()])) {
            platforms.insert(platform);
        }
    }
    if platforms.is_empty() {
        platforms.insert(Platform::LinuxX64);
    }
    for platform in Platform::ALL.into_iter().filter(|p| platforms.contains(p)) {
        let rel = format!(".github/picky/bin/{}", platform.file_name());
        let full = npath::join(&[root, &rel]);
        let current = std::fs::read(&full).ok();
        if let Some(bytes) = binaries.get(platform, current.as_deref())?
            && current.as_deref() != Some(bytes.as_slice())
        {
            files.push(InitFile { path: rel, before: current, after: Some(Content::Binary(bytes)), mode: Some(0o755) });
        }
    }
    let legacy = npath::join(&[root, LEGACY_BUNDLE]);
    if exists(&legacy) {
        files.push(InitFile { path: LEGACY_BUNDLE.into(), before: std::fs::read(&legacy).ok(), after: None, mode: None });
    }

    for (rel, text) in edited {
        add(&mut files, &rel, Content::Text(text), None);
    }
    Ok(InitPlan { files, notes })
}

fn exists(p: &str) -> bool {
    std::fs::metadata(p).is_ok()
}

/// A Node-style fs error message: `EISDIR: illegal operation on a directory, read`.
fn node_error(e: &std::io::Error, syscall: &str, path: &str) -> String {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT: no such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "EACCES: permission denied".to_string(),
        std::io::ErrorKind::IsADirectory => return "EISDIR: illegal operation on a directory, read".into(),
        std::io::ErrorKind::NotADirectory => "ENOTDIR: not a directory".to_string(),
        _ => e.to_string(),
    };
    format!("{code}, {syscall} '{path}'")
}

#[derive(Clone, Debug, PartialEq)]
struct Edit {
    at: usize,
    /// Bytes to replace, starting at `at`.
    remove: usize,
    text: String,
}

fn insert(at: usize, text: String) -> Edit {
    Edit { at, remove: 0, text }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowEdit {
    pub text: String,
    pub notes: Vec<String>,
    /// The platforms of the runners that run picky in this workflow.
    pub platforms: Vec<Platform>,
}

/// Marks the recognized commands of a workflow and wires in the hook. Err when reading the
/// workflow or the project throws.
pub fn edit_workflow(text: &str, root: &str) -> Result<WorkflowEdit, String> {
    let unchanged = |notes: Vec<String>| Ok(WorkflowEdit { text: text.to_string(), notes, platforms: Vec::new() });
    let mut notes: Vec<String> = Vec::new();
    let doc = parse_document(text);
    if let Some(e) = doc.errors.first() {
        return unchanged(vec![format!("not changed: invalid YAML ({})", e.message)]);
    }
    let workflow_value = doc.to_js()?;
    let workflow = Some(&workflow_value);
    if !runs_on_pull_requests(prop(workflow, "on")) {
        return unchanged(vec!["not changed: it does not run on pull_request".into()]);
    }

    let jobs_pair = match doc.contents.as_ref().map(|c| &c.kind) {
        Some(NodeKind::Map { items, .. }) => items.iter().find(|p| key_is(p, "jobs")),
        _ => None,
    };
    let (Some(jobs_key), Some(NodeKind::Map { items: job_items, .. })) =
        (jobs_pair.and_then(|p| p.key.as_ref()), jobs_pair.and_then(|p| p.value.as_ref()).map(|v| &v.kind))
    else {
        return unchanged(vec!["not changed: no jobs".into()]);
    };
    if job_items.is_empty() {
        return unchanged(vec!["not changed: no jobs".into()]);
    }
    let jobs_key_at = jobs_key.range[0];
    let js_jobs = prop(workflow, "jobs");

    let mut edits: Vec<Edit> = Vec::new();
    let mut picky_jobs: Vec<(String, &Node)> = Vec::new();
    let mut instances = 0;
    let mut marks = 0;

    for pair in job_items {
        let name = key_string(pair);
        let job = prop(js_jobs, &name);
        let Some(map) = pair.value.as_ref().filter(|v| matches!(v.kind, NodeKind::Map { .. })) else { continue };
        if !truthy(job) || name == "picky" {
            continue;
        }
        if truthy(prop(job, "uses")) {
            continue; // a reusable workflow call has no steps here
        }
        let windows = test(&WINDOWS, &string_of(prop(job, "runs-on")));
        // The hook needs bash and a host path: Windows and container jobs use `shell: picky {0}` instead.
        let hookable = !windows && !truthy(prop(job, "container"));
        let Some(steps) = map.get("steps") else { continue };
        let NodeKind::Seq { items: step_nodes, .. } = &steps.kind else { continue };
        let js_steps = prop(job, "steps");

        let mut job_edits: Vec<Edit> = Vec::new();
        let mut marked = false;
        let mut via_shell = false;
        let mut recognized_any = false;
        let mut fallback: Vec<(&Node, &Node, String)> = Vec::new();
        for (i, node) in step_nodes.iter().enumerate() {
            let step = prop(js_steps, &i.to_string());
            if !matches!(node.kind, NodeKind::Map { .. }) {
                continue;
            }
            let Some(Value::String(script)) = prop(step, "run") else { continue };
            let dir_value = coalesce(&[
                prop(step, "working-directory"),
                prop(run_defaults(job), "working-directory"),
                prop(run_defaults(workflow), "working-directory"),
            ]);
            let dir = if nullish(dir_value) { ".".to_string() } else { string_of(dir_value) };
            if dir.contains("${{") {
                continue;
            }
            let Some(run) = node.get("run").filter(|r| matches!(r.kind, NodeKind::Scalar { .. })) else { continue };
            let cwd = npath::resolve(&[root, &dir]);
            let step_shell = prop(step, "shell");
            let shell = coalesce(&[step_shell, prop(run_defaults(job), "shell"), prop(run_defaults(workflow), "shell")]);
            if let Some(Value::String(s)) = step_shell
                && test(&PICKY_SHELL, s)
            {
                marked = true;
                via_shell = true;
                continue;
            }

            if hookable && (shell.is_none() || test(&BASH_SHELL, &string_of(shell))) {
                let comment = run.comment.as_deref().unwrap_or("");
                if test(&MARKER_COMMENT, comment) || scan_script(script, false).iter().any(|l| l.marked) {
                    marked = true;
                    continue;
                }
                let found = mark_run(text, run, script, &cwd)?;
                if !found.is_empty() {
                    marked = true;
                }
                marks += found.len();
                job_edits.extend(found);
                continue;
            }

            // `shell: picky [shell] {0}` runs the whole step; any other custom shell is left alone.
            let inner = match shell {
                None => (if windows { "pwsh" } else { "" }).to_string(),
                Some(v) => trim(&js_string(v)).to_string(),
            };
            if !inner.is_empty() && Shell::parse(Some(&inner)).is_none() {
                continue;
            }
            let words: Vec<Vec<String>> = scan_script(script, false)
                .iter()
                .filter(|l| l.simple)
                .filter_map(|l| tokenize(&l.text))
                .filter(|w| !w.is_empty())
                .collect();
            let mut recognized = false;
            for w in &words {
                if recognize(w, &cwd)?.is_some() {
                    recognized = true;
                    break;
                }
            }
            recognized_any |= recognized;
            if recognized || words.iter().any(|w| is_setup_command(Some(w.as_slice()))) {
                fallback.push((node, run, inner));
            }
        }

        if recognized_any {
            match setup_action_step(text, step_nodes, js_steps) {
                SetupStep::NoCheckout => {
                    notes.push(format!("job \"{name}\" skipped: it needs actions/checkout before picky's setup step"));
                }
                setup => {
                    if let SetupStep::Add(edit) = setup {
                        job_edits.push(edit);
                    }
                    for (node, run, inner) in &fallback {
                        job_edits.push(shell_edit(text, node, run, inner));
                    }
                    marks += fallback.len();
                    marked = true;
                }
            }
        } else if via_shell
            && !has_setup_action(js_steps)
            && let SetupStep::Add(edit) = setup_action_step(text, step_nodes, js_steps)
        {
            job_edits.push(edit);
        }

        if marked {
            edits.extend(job_edits);
            picky_jobs.push((name, map));
            instances += if truthy(prop(prop(job, "strategy"), "matrix")) { 2 } else { 1 };
        }
    }

    if picky_jobs.is_empty() {
        notes.push("not changed: no step runs a command picky recognizes".into());
        return unchanged(notes);
    }

    match env_edits(text, doc.get("env"), jobs_key_at) {
        Ok(env) => edits.extend(env),
        Err(note) => notes.push(note),
    }

    let mut platforms: IndexSet<Platform> = IndexSet::new();
    let mut unknown_runners: Vec<String> = Vec::new();
    let mut add_runner = |name: &str, job: Option<&Value>| match runner_platforms(job) {
        Some(found) => platforms.extend(found),
        None => {
            unknown_runners.push(name.to_string());
            platforms.extend([Platform::LinuxX64, Platform::WindowsX64, Platform::DarwinArm64]);
        }
    };
    for (name, _) in &picky_jobs {
        add_runner(name, prop(js_jobs, name));
    }

    // Several jobs (or matrix legs) would each ask the model; one shared decision keeps them consistent.
    if instances > 1 {
        let decide_job = prop(js_jobs, "picky");
        if truthy(decide_job) {
            add_runner("picky", decide_job);
        } else {
            edits.push(decide_job_edit(text, job_items, jobs_key_at));
            add_runner("picky", None);
        }
        for (name, map) in &picky_jobs {
            let job = prop(js_jobs, name);
            let needs = prop(job, "needs");
            let has_picky = match needs {
                Some(Value::Array(items)) => items.iter().any(|n| *n == Value::String("picky".into())),
                other => other == Some(&Value::String("picky".into())),
            };
            if !has_picky {
                match needs_picky(text, map) {
                    Some(added) => edits.push(added),
                    None => notes.push(format!("job \"{name}\": add `picky` to its needs by hand")),
                }
            }
            let downloads = match prop(job, "steps") {
                Some(Value::Array(steps)) => steps.iter().any(|s| {
                    string_of(prop(Some(s), "uses")).starts_with("actions/download-artifact@")
                        && prop(prop(Some(s), "with"), "name") == Some(&Value::String("picky-decision".into()))
                }),
                _ => false,
            };
            if !downloads && let Some(step) = download_step(text, map, truthy(prop(job, "container"))) {
                edits.push(step);
            }
        }
    }
    for name in unknown_runners {
        notes.push(format!(
            "job \"{name}\": can't tell its runner from `runs-on`; vendoring the linux-x64, windows-x64 and darwin-arm64 binaries"
        ));
    }
    let n = picky_jobs.len();
    notes.push(format!("marked {marks} command{} in {n} job{}", plural(marks), plural(n)));
    Ok(WorkflowEdit { text: apply(text, edits), notes, platforms: platforms.into_iter().collect() })
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `p.key?.value === key`
fn key_is(p: &Pair, key: &str) -> bool {
    matches!(&p.key, Some(Node { kind: NodeKind::Scalar { value: Value::String(s), .. }, .. }) if s == key)
}

/// `String(pair.key?.value)`
fn key_string(p: &Pair) -> String {
    match &p.key {
        Some(Node { kind: NodeKind::Scalar { value, .. }, .. }) => js_string(value),
        _ => "undefined".into(),
    }
}

/// The platforms a job's runner can be, from `runs-on` and the matrix values it names. None when
/// picky can't tell (a runner group, an expression other than `${{ matrix.<key> }}`).
fn runner_platforms(job: Option<&Value>) -> Option<Vec<Platform>> {
    let Some(job) = job else { return Some(vec![Platform::LinuxX64]) }; // the decide job picky adds
    let runs_on = prop(Some(job), "runs-on")?;
    let labels: Vec<&Value> = match runs_on {
        Value::Object(_) => match prop(Some(runs_on), "labels")? {
            Value::Array(items) => items.iter().collect(),
            single => vec![single],
        },
        Value::Array(items) => items.iter().collect(),
        single => vec![single],
    };
    // Each label can expand to several (a matrix); a runner is one choice for every label.
    let mut runners: Vec<Vec<String>> = vec![Vec::new()];
    for label in labels {
        let choices = label_choices(job, label)?;
        runners = runners
            .iter()
            .flat_map(|runner| choices.iter().map(move |choice| runner.iter().chain(choice).cloned().collect::<Vec<String>>()))
            .collect();
        if runners.len() > 256 {
            return None;
        }
    }
    let mut out: Vec<Platform> = Vec::new();
    for runner in runners {
        let platform = runner_platform(&runner);
        if !out.contains(&platform) {
            out.push(platform);
        }
    }
    Some(out)
}

/// The labels one `runs-on` entry can stand for.
fn label_choices(job: &Value, label: &Value) -> Option<Vec<Vec<String>>> {
    let Value::String(s) = label else { return scalar_labels(label).map(|l| vec![l]) };
    if !s.contains("${{") {
        return Some(vec![vec![s.clone()]]);
    }
    let u = units(s);
    let m = MATRIX_EXPRESSION.exec(&u)?;
    let path: Vec<String> = crate::jsre::from_units(m.str(1)).split('.').map(String::from).collect();
    let matrix = prop(prop(Some(job), "strategy"), "matrix")?;
    if !matches!(matrix, Value::Object(_)) {
        return None;
    }
    let mut values: Vec<&Value> = match prop(Some(matrix), &path[0]) {
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::String(e)) if e.contains("${{") => return None,
        Some(v) => vec![v],
        None => Vec::new(),
    };
    if let Some(Value::Array(include)) = prop(Some(matrix), "include") {
        values.extend(include.iter().filter_map(|entry| prop(Some(entry), &path[0])));
    }
    if values.is_empty() {
        return None;
    }
    values.into_iter().map(|v| path[1..].iter().try_fold(v, |v, key| prop(Some(v), key)).and_then(scalar_labels)).collect()
}

/// A label value: a string, or an array of them.
fn scalar_labels(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::String(s) if !s.contains("${{") => Some(vec![s.clone()]),
        Value::Array(items) => items.iter().map(|i| scalar_labels(i).filter(|l| l.len() == 1).map(|mut l| l.remove(0))).collect(),
        _ => None,
    }
}

/// The platform of a runner with these labels (GitHub-hosted names, or self-hosted OS and CPU labels).
fn runner_platform(labels: &[String]) -> Platform {
    let any = |re: &JsRegex| labels.iter().any(|l| test(re, l));
    if any(&WINDOWS) {
        Platform::WindowsX64
    } else if any(&MACOS) {
        if any(&MACOS_ARM) || !any(&MACOS_INTEL) { Platform::DarwinArm64 } else { Platform::DarwinX64 }
    } else if any(&ARM) {
        Platform::LinuxArm64
    } else {
        Platform::LinuxX64
    }
}

fn has_setup_action(steps: Option<&Value>) -> bool {
    match steps {
        Some(Value::Array(items)) => items.iter().any(|s| test(&SETUP_ACTION_USES, &string_of(prop(Some(s), "uses")))),
        _ => false,
    }
}

enum SetupStep {
    Add(Edit),
    /// Already there.
    Present,
    /// No checkout to follow.
    NoCheckout,
}

/// Adds `uses: ./.github/picky` right after checkout.
fn setup_action_step(text: &str, steps: &[Node], js_steps: Option<&Value>) -> SetupStep {
    if has_setup_action(js_steps) {
        return SetupStep::Present;
    }
    let Some(Value::Array(items)) = js_steps else { return SetupStep::NoCheckout };
    let checkout = items.iter().position(|s| string_of(prop(Some(s), "uses")).starts_with("actions/checkout@"));
    let Some(next) = checkout.and_then(|i| steps.get(i + 1)) else { return SetupStep::NoCheckout };
    let at = line_start(text, next.range[0]);
    SetupStep::Add(insert(at, format!("{}uses: ./.github/picky\n", &text[at..next.range[0]])))
}

/// Runs the step through picky: `shell: picky [shell] {0}`, replacing or adding the step's `shell`.
fn shell_edit(text: &str, step: &Node, run: &Node, inner: &str) -> Edit {
    let value = if inner.is_empty() { "picky {0}".to_string() } else { format!("picky {inner} {{0}}") };
    if let Some(shell) = step.get("shell")
        && matches!(shell.kind, NodeKind::Scalar { .. })
    {
        return Edit { at: shell.range[0], remove: shell.range[1] - shell.range[0], text: value };
    }
    let NodeKind::Map { items, .. } = &step.kind else { unreachable!("steps are maps") };
    let run_key = items.iter().find(|p| key_is(p, "run")).and_then(|p| p.key.as_ref()).expect("the run key");
    let indent = " ".repeat(column(text, run_key.range[0]));
    let end = run.range[1];
    // A block value already ends after its last line break.
    let at = if end > 0 && text.as_bytes()[end - 1] == b'\n' { end } else { (line_end(text, end) + 1).min(text.len()) };
    let lead = if at == text.len() && !text.ends_with('\n') { "\n" } else { "" };
    insert(at, format!("{lead}{indent}shell: {value}\n"))
}

fn runs_on_pull_requests(on: Option<&Value>) -> bool {
    let pull_request = Value::String("pull_request".into());
    match on {
        Some(Value::String(s)) => s == "pull_request",
        Some(Value::Array(items)) => items.contains(&pull_request),
        Some(Value::Object(entries)) => entries.iter().any(|(k, _)| k == "pull_request"),
        _ => false,
    }
}

/// Edits that add `# picky` to each recognized command of a `run:` value.
fn mark_run(text: &str, run: &Node, script: &str, cwd: &str) -> Result<Vec<Edit>, String> {
    let recognized = |line: &str| -> Result<bool, String> {
        match tokenize(line) {
            Some(words) if !words.is_empty() => Ok(recognize(&words, cwd)?.is_some()),
            _ => Ok(false),
        }
    };
    let NodeKind::Scalar { style, .. } = &run.kind else { return Ok(Vec::new()) };

    if *style == ScalarStyle::Plain {
        let end = run.range[1];
        // A one-line value: the marker goes in as a YAML comment. An existing comment is left alone.
        if script.contains('\n') || text[end..line_end(text, end)].contains('#') {
            return Ok(Vec::new());
        }
        return Ok(if recognized(script)? { vec![insert(end, " # picky".into())] } else { Vec::new() });
    }
    if *style != ScalarStyle::BlockLiteral {
        return Ok(Vec::new());
    }

    // Literal block lines map one to one onto the source lines after the `|` header.
    let first = line_end(text, run.range[0]) + 1;
    let body = if first < run.range[1] { &text[first..run.range[1]] } else { "" };
    let source: Vec<&str> = body.split('\n').collect();
    let lines: Vec<&str> = script.split('\n').collect();
    let mut edits = Vec::new();
    for line in scan_script(script, false) {
        let at = line.line - 1;
        let (Some(src), Some(original)) = (source.get(at), lines.get(at)) else { return Ok(Vec::new()) };
        if trim(src) != trim(original) {
            return Ok(Vec::new()); // unexpected layout: change nothing
        }
        if line.simple && recognized(&line.text)? {
            let start = first + source[..at].iter().map(|l| l.len() + 1).sum::<usize>();
            edits.push(insert(start + trim_end(src).len(), " # picky".into()));
        }
    }
    Ok(edits)
}

/// Adds the hook's env lines at the top of the workflow, keeping any that exist. Err is a note.
fn env_edits(text: &str, env: Option<&Node>, jobs_key_at: usize) -> Result<Vec<Edit>, String> {
    let lines = |indent: &str, keys: &[&str]| -> String {
        ENV.iter().filter(|(k, _)| keys.contains(k)).map(|(k, v)| format!("{indent}{k}: {v}\n")).collect()
    };
    let all: Vec<&str> = ENV.iter().map(|(k, _)| *k).collect();
    let Some(env) = env else {
        return Ok(vec![insert(line_start(text, jobs_key_at), format!("env:\n{}\n", lines("  ", &all)))]);
    };
    let by_hand = || Err("add BASH_ENV and PICKY_API_KEY to its top-level env by hand".to_string());
    let NodeKind::Map { items, flow } = &env.kind else { return by_hand() };
    if *flow || items.is_empty() {
        return by_hand();
    }
    let present: IndexSet<String> = items.iter().map(key_string).collect();
    let missing: Vec<&str> = all.into_iter().filter(|k| !present.contains(*k)).collect();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let last = items.last().expect("non-empty");
    let (Some(end), Some(first_key)) = (last.value.as_ref().or(last.key.as_ref()), items[0].key.as_ref()) else {
        return by_hand();
    };
    let indent = " ".repeat(column(text, first_key.range[0]));
    Ok(vec![insert(line_end(text, end.range[1]) + 1, lines(&indent, &missing))])
}

/// The decide job goes first, right under `jobs:`, so a comment above the first job stays with it.
fn decide_job_edit(text: &str, jobs: &[Pair], jobs_key_at: usize) -> Edit {
    let key_at = jobs[0].key.as_ref().map_or(jobs_key_at, |k| k.range[0]);
    let i = " ".repeat(column(text, key_at));
    let job = [
        format!("{i}# Computes the picky decision once; the jobs that need it reuse it. Never fails the run."),
        format!("{i}picky:"),
        format!("{i}  runs-on: ubuntu-latest"),
        format!("{i}  steps:"),
        format!("{i}    - uses: actions/checkout@v4"),
        format!("{i}      continue-on-error: true"),
        format!("{i}    - run: .github/picky/picky decide"),
        format!("{i}      continue-on-error: true"),
        format!("{i}    - uses: actions/upload-artifact@v4"),
        format!("{i}      continue-on-error: true"),
        format!("{i}      with:"),
        format!("{i}        name: picky-decision"),
        format!("{i}        path: ${{{{ runner.temp }}}}/picky"),
        format!("{i}        retention-days: 7"),
        String::new(),
        String::new(),
    ]
    .join("\n");
    insert(line_end(text, jobs_key_at) + 1, job)
}

/// Adds `picky` to a job's `needs`, or None when its form isn't one picky can edit safely.
fn needs_picky(text: &str, job: &Node) -> Option<Edit> {
    let NodeKind::Map { items, .. } = &job.kind else { return None };
    let Some(needs) = job.get("needs") else {
        let first_key = items.first()?.key.as_ref()?;
        let at = line_start(text, first_key.range[0]);
        return Some(insert(at, format!("{}needs: picky\n", " ".repeat(column(text, first_key.range[0])))));
    };
    match &needs.kind {
        NodeKind::Scalar { value, style: ScalarStyle::Plain } => {
            let [start, end, _] = needs.range;
            Some(Edit { at: start, remove: end - start, text: format!("[{}, picky]", js_string(value)) })
        }
        NodeKind::Seq { items, flow } if !items.is_empty() => {
            if *flow {
                let upto = (needs.range[1] + 1).min(text.len());
                return Some(insert(text[..upto].rfind(']')?, ", picky".into()));
            }
            let last = items.last()?;
            let at = line_start(text, last.range[0]);
            let dash = &text[at..last.range[0]];
            Some(insert(line_end(text, last.range[1]) + 1, format!("{dash}picky\n")))
        }
        _ => None,
    }
}

/// Downloads the shared decision before the job's first step. A missing artifact is ignored.
fn download_step(text: &str, job: &Node, container: bool) -> Option<Edit> {
    let steps = job.get("steps")?;
    let NodeKind::Seq { items, flow: false } = &steps.kind else { return None };
    let first = items.first()?;
    let at = line_start(text, first.range[0]);
    let dash = &text[at..first.range[0]]; // e.g. "      - "
    let i = " ".repeat(dash.encode_utf16().count());
    // In a container, `runner.temp` is still the host path; the runner mounts it at /__w/_temp.
    let path = if container { "/__w/_temp/picky" } else { "${{ runner.temp }}/picky" };
    Some(insert(
        at,
        [
            format!("{dash}uses: actions/download-artifact@v4"),
            format!("{i}continue-on-error: true"),
            format!("{i}with:"),
            format!("{i}  name: picky-decision"),
            format!("{i}  path: {path}"),
            String::new(),
        ]
        .join("\n"),
    ))
}

fn apply(text: &str, mut edits: Vec<Edit>) -> String {
    let mut out = text.to_string();
    edits.sort_by_key(|e| std::cmp::Reverse(e.at));
    for e in edits {
        let start = e.at.min(out.len());
        let end = (e.at + e.remove).min(out.len());
        out.replace_range(start..end, &e.text);
    }
    out
}

/// `text.lastIndexOf('\n', offset - 1) + 1`
fn line_start(text: &str, offset: usize) -> usize {
    let upto = (offset.saturating_sub(1) + 1).min(text.len());
    text.as_bytes()[..upto].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
}

fn line_end(text: &str, offset: usize) -> usize {
    text.get(offset..).and_then(|rest| rest.find('\n')).map_or(text.len(), |i| offset + i)
}

/// The column of `offset`, in UTF-16 units like the TypeScript version counted.
fn column(text: &str, offset: usize) -> usize {
    text[line_start(text, offset)..offset].encode_utf16().count()
}

/// The release binaries: from a directory (`--dist`), this binary when it is the release build
/// for that platform, else downloaded from the GitHub release and checked against its SHA256SUMS.
pub struct ReleaseBinaries {
    pub dist: Option<String>,
    pub log: Box<dyn Fn(&str)>,
    sums: Option<String>,
}

impl ReleaseBinaries {
    pub fn new(dist: Option<String>, log: Box<dyn Fn(&str)>) -> Self {
        ReleaseBinaries { dist, log, sums: None }
    }

    fn url(name: &str) -> String {
        format!("https://github.com/ahmedragab20/picky/releases/download/v{}/{name}", env!("CARGO_PKG_VERSION"))
    }

    fn download(name: &str) -> Result<Vec<u8>, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(120))).build().into();
        let url = Self::url(name);
        let mut res = agent.get(&url).call().map_err(|e| format!("can't download {url} ({e})"))?;
        res.body_mut().with_config().limit(200 * 1024 * 1024).read_to_vec().map_err(|e| format!("can't download {url} ({e})"))
    }
}

/// The platform this binary was released for, set by the release build.
pub const RELEASE_PLATFORM: Option<&str> = option_env!("PICKY_RELEASE_PLATFORM");

impl Binaries for ReleaseBinaries {
    fn get(&mut self, platform: Platform, current: Option<&[u8]>) -> Result<Option<Vec<u8>>, String> {
        let name = platform.file_name();
        if let Some(dist) = &self.dist {
            let file = npath::join(&[dist, &name]);
            return std::fs::read(&file).map(Some).map_err(|e| format!("can't read {name} ({})", node_error(&e, "open", &file)));
        }
        if RELEASE_PLATFORM == Some(platform.as_str())
            && let Ok(bytes) = std::env::current_exe().and_then(std::fs::read)
        {
            return Ok(Some(bytes));
        }
        if self.sums.is_none() {
            let sums = Self::download("SHA256SUMS")?;
            self.sums = Some(String::from_utf8_lossy(&sums).into_owned());
        }
        let sums = self.sums.as_deref().unwrap_or("");
        let want = sums
            .lines()
            .find_map(|l| l.split_once(char::is_whitespace).filter(|(_, f)| f.trim_start().trim_start_matches('*') == name))
            .map(|(hash, _)| hash.to_ascii_lowercase())
            .ok_or_else(|| format!("{name} is not in {}", Self::url("SHA256SUMS")))?;
        if current.is_some_and(|c| format!("{:x}", Sha256::digest(c)) == want) {
            return Ok(None);
        }
        (self.log)(&format!("picky: downloading {}", Self::url(&name)));
        let bytes = Self::download(&name)?;
        let got = format!("{:x}", Sha256::digest(&bytes));
        if got != want {
            return Err(format!("{name}: checksum mismatch (expected {want}, got {got})"));
        }
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_lines_like_the_typescript_helpers() {
        let text = "a\nbc\n\nd";
        assert_eq!(line_start(text, 0), 0);
        assert_eq!(line_start(text, 3), 2);
        assert_eq!(line_start(text, 2), 2);
        assert_eq!(line_start(text, 5), 5);
        assert_eq!(line_end(text, 2), 4);
        assert_eq!(line_end(text, 6), 7);
        assert_eq!(line_end(text, 99), 7);
        assert_eq!(column("é: x\n  ab", 8), 2);
        assert_eq!(column("é: x", 4), 3);
    }

    #[test]
    fn applies_edits_from_the_end() {
        let edits = vec![insert(1, "X".into()), Edit { at: 2, remove: 1, text: "Y".into() }, insert(1, "Z".into())];
        // Stable like Array.prototype.sort: of two edits at one offset, the later one ends up first.
        assert_eq!(apply("abcd", edits), "aZXbYd");
        assert_eq!(apply("ab", vec![insert(5, "!".into())]), "ab!");
    }

    #[test]
    fn tells_runners_apart() {
        let p = |labels: &[&str]| runner_platform(&labels.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(p(&["ubuntu-latest"]), Platform::LinuxX64);
        assert_eq!(p(&["ubuntu-24.04-arm"]), Platform::LinuxArm64);
        assert_eq!(p(&["self-hosted", "linux", "ARM64"]), Platform::LinuxArm64);
        assert_eq!(p(&["warm-pool"]), Platform::LinuxX64);
        assert_eq!(p(&["windows-latest"]), Platform::WindowsX64);
        assert_eq!(p(&["windows-11-arm"]), Platform::WindowsX64);
        assert_eq!(p(&["macos-latest"]), Platform::DarwinArm64);
        assert_eq!(p(&["macos-15"]), Platform::DarwinArm64);
        assert_eq!(p(&["macos-13"]), Platform::DarwinX64);
        assert_eq!(p(&["macos-13-xlarge"]), Platform::DarwinArm64);
        assert_eq!(p(&["macos-14-large"]), Platform::DarwinX64);
        assert_eq!(p(&["macos-15-intel"]), Platform::DarwinX64);
        assert_eq!(p(&["self-hosted", "macOS", "X64"]), Platform::DarwinX64);
        assert_eq!(p(&["self-hosted", "macOS"]), Platform::DarwinArm64);
    }

    #[test]
    fn reads_runners_from_the_matrix() {
        let job = |yaml: &str| crate::yaml::parse(yaml).unwrap();
        let platforms = |yaml: &str| runner_platforms(Some(&job(yaml)));
        assert_eq!(platforms("runs-on: ubuntu-latest"), Some(vec![Platform::LinuxX64]));
        assert_eq!(
            platforms("runs-on: ${{ matrix.os }}\nstrategy:\n  matrix:\n    os: [ubuntu-latest, windows-latest, macos-13]"),
            Some(vec![Platform::LinuxX64, Platform::WindowsX64, Platform::DarwinX64])
        );
        assert_eq!(
            platforms(
                "runs-on: ${{ matrix.target.runner }}\nstrategy:\n  matrix:\n    target:\n      - { runner: ubuntu-24.04-arm }\n    include:\n      - target: { runner: macos-latest }"
            ),
            Some(vec![Platform::LinuxArm64, Platform::DarwinArm64])
        );
        assert_eq!(platforms("runs-on: [self-hosted, linux, arm64]"), Some(vec![Platform::LinuxArm64]));
        assert_eq!(platforms("runs-on: { group: big, labels: [windows-2022] }"), Some(vec![Platform::WindowsX64]));
        assert_eq!(platforms("runs-on: { group: big }"), None);
        assert_eq!(platforms("runs-on: ${{ inputs.os }}"), None);
        assert_eq!(platforms("runs-on: ${{ matrix.os }}\nstrategy:\n  matrix: ${{ fromJSON(inputs.m) }}"), None);
        assert_eq!(platforms("runs-on: ${{ matrix.os }}\nstrategy:\n  matrix:\n    node: [20]"), None);
        assert_eq!(platforms("steps: []"), None);
    }
}
