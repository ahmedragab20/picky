//! `shell: picky {0}`: the fallback for steps the bash hook can't reach (Windows, containers,
//! other shells). GitHub hands picky the step script; picky decides every command in it, then
//! runs the result in the real shell the way GitHub would have.

use std::sync::LazyLock;

use crate::hook::scan_script;
use crate::job::JobPlan;
use crate::js::{trim, trim_end};
use crate::jsre::{JsRegex, from_units, units};
use crate::plan::{Enforce, Outcome};
use crate::report::shadow_miss_message;
use crate::sh::{quote_argv, split_assignments, tokenize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Sh,
    Pwsh,
    Powershell,
}

pub const SHELLS: &[&str] = &["bash", "sh", "pwsh", "powershell"];

impl Shell {
    pub fn parse(name: Option<&str>) -> Option<Shell> {
        match name? {
            "bash" => Some(Shell::Bash),
            "sh" => Some(Shell::Sh),
            "pwsh" => Some(Shell::Pwsh),
            "powershell" => Some(Shell::Powershell),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Sh => "sh",
            Shell::Pwsh => "pwsh",
            Shell::Powershell => "powershell",
        }
    }

    fn is_posix(self) -> bool {
        matches!(self, Shell::Bash | Shell::Sh)
    }
}

/// GitHub's default shell when the step names none: pwsh on Windows, else bash when present, else sh.
pub fn default_shell(windows: bool) -> Shell {
    if windows {
        return Shell::Pwsh;
    }
    let bash = std::process::Command::new("bash")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if bash.is_ok_and(|s| s.success()) { Shell::Bash } else { Shell::Sh }
}

pub fn extension(shell: Shell) -> &'static str {
    if shell.is_posix() { ".sh" } else { ".ps1" }
}

/// The command line GitHub uses for each built-in shell.
pub fn invocation(shell: Shell, file: &str) -> (String, Vec<String>) {
    match shell {
        Shell::Bash => ("bash".into(), ["--noprofile", "--norc", "-eo", "pipefail", file].map(String::from).to_vec()),
        Shell::Sh => ("sh".into(), vec!["-e".into(), file.into()]),
        _ => (shell.as_str().into(), vec!["-command".into(), format!(". '{}'", file.replace('\'', "''"))]),
    }
}

/// GitHub wraps PowerShell scripts: stop on errors, and exit with the last native command's code.
pub fn finalize(script: &str, shell: Shell) -> String {
    if shell.is_posix() {
        return script.to_string();
    }
    format!("$ErrorActionPreference = 'stop'\n{script}\nif ((Test-Path -LiteralPath variable:\\LASTEXITCODE)) {{ exit $LASTEXITCODE }}\n")
}

pub fn quote_for<S: AsRef<str>>(shell: Shell, argv: &[S]) -> String {
    if shell.is_posix() {
        return quote_argv(argv);
    }
    let safe = |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-./:=+,".contains(&b));
    argv.iter()
        .map(|w| {
            let w = w.as_ref();
            if safe(w) { w.to_string() } else { format!("'{}'", w.replace('\'', "''")) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Shadow mode: the command runs in full, in the step's own shell. If it fails, the line checks it
/// against what enforce mode would have run, and picky (`self_`) only reports. The exit code stays.
pub fn shadow_line(shell: Shell, words: &[String], enforce: &Enforce, self_: &[String]) -> String {
    let full = quote_for(shell, words);
    // PowerShell has no `VAR=value cmd` form.
    if let Enforce::Narrow { argv, .. } = enforce
        && !shell.is_posix()
        && !split_assignments(argv).env.is_empty()
    {
        return full;
    }
    let message = shadow_miss_message(words, enforce);
    let narrowed = match enforce {
        Enforce::Narrow { argv, .. } => quote_for(shell, argv),
        Enforce::Skip { .. } => String::new(),
    };
    let command = |args: &[&str]| -> Vec<String> {
        self_.iter().cloned().chain(std::iter::once("shadow-report".to_string())).chain(args.iter().map(|a| a.to_string())).collect()
    };
    if shell.is_posix() {
        let report = |args: &[&str]| format!("{} || true", quote_argv(&command(args)));
        let check = match enforce {
            Enforce::Skip { .. } => report(&["skipped", &message]),
            Enforce::Narrow { .. } => format!(
                "{}; if {narrowed}; then {}; else {}; fi",
                report(&["check", &narrowed]),
                report(&["missed", &message]),
                report(&["caught"])
            ),
        };
        return format!("{full} || {{ __picky_rc=$?; {check}; (exit $__picky_rc); }}");
    }
    let report = |args: &[&str]| format!("& {}", quote_for(shell, &command(args)));
    let check = match enforce {
        Enforce::Skip { .. } => report(&["skipped", &message]),
        Enforce::Narrow { .. } => format!(
            "{}; {narrowed}; if ($LASTEXITCODE) {{ {} }} else {{ {} }}",
            report(&["check", &narrowed]),
            report(&["caught"]),
            report(&["missed", &message])
        ),
    };
    format!("{full}; if ($LASTEXITCODE) {{ $__picky_rc = $LASTEXITCODE; {check}; $global:LASTEXITCODE = $__picky_rc }}")
}

pub trait StepPlanner {
    fn line(&mut self, words: &[String]) -> Outcome;
    /// The job-level rule for setup commands; asked at most once per step.
    fn setup(&mut self) -> Result<JobPlan, String>;
    /// The command line that runs picky, for shadow-mode lines.
    fn self_command(&self) -> Vec<String>;
}

static LEADING_SPACE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^\s*", ""));
static MARKER_TAIL: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\s*#\s*picky.*$", ""));

/// `text.trim().replace(/\s*#\s*picky.*$/, '')`
pub fn label_of(text: &str) -> String {
    let u = units(trim(text)).into_owned();
    from_units(&MARKER_TAIL.replace_first(&u, |_| String::new())).into_owned()
}

/// Decides every command of a step: recognized ones are skipped, narrowed or kept; setup commands follow the job rule.
pub fn rewrite_step(script: &str, shell: Shell, planner: &mut dyn StepPlanner, log: &dyn Fn(&str)) -> Result<String, String> {
    let text = script.replace("\r\n", "\n");
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let noop = if shell.is_posix() { ": skipped by picky" } else { "# skipped by picky" };
    let mut setup: Option<JobPlan> = None;

    for line in scan_script(&text, true) {
        // PowerShell continues lines with a backtick.
        let previous = line.line.checked_sub(2).and_then(|i| lines.get(i)).map_or("", String::as_str);
        let continued = !shell.is_posix() && (trim_end(&line.text).ends_with('`') || trim_end(previous).ends_with('`'));
        let words = if line.simple && !continued { tokenize(&line.text) } else { None };
        let Some(words) = words.filter(|w| !w.is_empty()) else { continue };
        let u = units(&line.text).into_owned();
        let indent = from_units(LEADING_SPACE.exec(&u).map_or("", |m| m.str(0))).into_owned();
        let label = label_of(&line.text);
        let at = line.line - 1;

        if !line.marked {
            // Under a whole-step marker only setup commands stay unmarked.
            if setup.is_none() {
                setup = Some(planner.setup()?);
            }
            let plan = setup.as_ref().expect("set");
            log(&format!("picky: {label} -> {}", plan.note));
            if plan.skip {
                lines[at] = format!("{indent}{noop}");
            }
            continue;
        }

        let outcome = planner.line(&words);
        if matches!(&outcome, Outcome::Full { note, .. } if note == "command not recognized; runs in full") {
            continue;
        }
        log(&format!("picky: {label} -> {}", outcome.note()));
        match &outcome {
            Outcome::Skip { .. } => lines[at] = format!("{indent}{noop}"),
            Outcome::Full { enforce: Some(enforce), .. } => {
                lines[at] = format!("{indent}{}", shadow_line(shell, &words, enforce, &planner.self_command()));
            }
            Outcome::Narrow { argv, .. } => {
                // PowerShell has no `VAR=value cmd` form.
                if !shell.is_posix() && !split_assignments(argv).env.is_empty() {
                    continue;
                }
                log(&format!("picky: $ {}", quote_for(shell, argv)));
                lines[at] = format!("{indent}{}", quote_for(shell, argv));
            }
            Outcome::Full { enforce: None, .. } => {}
        }
    }
    Ok(lines.join("\n"))
}
