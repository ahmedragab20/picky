use std::io::Write;
use std::process::{Command, ExitCode};

use picky::Env;
use picky::decide::{DecideOptions, Decision, decide_cached};
use picky::git::{exists, repo_root, spawn_error_code};
use picky::hook::{bash_flags, rewrite_script, scan_script};
use picky::init::{Content, InitFile, ReleaseBinaries, plan_init};
use picky::job::{JobPlan, current_step, load_job, plan_setup};
use picky::js::{parse_number, to_fixed, trim, trim_end};
use picky::npath;
use picky::plan::{Enforce, Outcome, plan_command};
use picky::report::{format_decision, shadow_miss_message, shadow_report, write_job_summary};
use picky::sh::{quote_argv, split_assignments, tokenize};
use picky::shell::{Shell, StepPlanner, default_shell, extension, finalize, invocation, label_of, rewrite_step, shadow_line};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "picky: runs only the tests, lint and type checks a change can affect (JS/TS, PHP, Kotlin and Rust)

usage:
  picky init [--write] [--dist <dir>]   show (or apply) the files and workflow edits that turn picky on
  picky explain                         show the decision for the current branch
  picky decide                          compute the decision once, for other jobs to reuse
  picky -- <command...>                 run a test, lint or check command, narrowed to the change
  picky shadow-report <kind> [arg]      report a shadow-mode check (used by the step rewrites)
  picky hook-exec <script> [args...]    run a CI step script, handling '# picky' lines
  picky rewrite --script <s> --line <n> print the shell text for one marked line
  picky [bash|sh|pwsh|powershell] <script>
                                        run a whole step: `shell: picky {0}` in a workflow
  picky --version                       print the version

environment:
  PICKY_MODE=shadow|enforce   PICKY_MODEL=jev|typesafe/<version>|none
  PICKY_API_KEY               PICKY_BASE=<ref>   PICKY_BUDGET_SECONDS=30";

fn log(msg: &str) {
    let _ = writeln!(std::io::stderr(), "{msg}");
}

fn out(msg: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(msg.as_bytes());
    let _ = stdout.flush();
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let env: Env = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
    let cli = Cli { env };
    let code = cli.main(&argv).unwrap_or_else(|msg| {
        log(&format!("picky: {msg}"));
        1
    });
    ExitCode::from(code)
}

struct Cli {
    env: Env,
}

fn cwd() -> String {
    std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".into())
}

fn read_text(file: &str) -> Result<String, String> {
    std::fs::read(file).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| fs_error(&e, "open", file))
}

/// Node's fs error text: `ENOENT: no such file or directory, open 'x'`.
fn fs_error(e: &std::io::Error, syscall: &str, path: &str) -> String {
    let what = match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT: no such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "EACCES: permission denied".to_string(),
        std::io::ErrorKind::IsADirectory => return "EISDIR: illegal operation on a directory, read".into(),
        _ => e.to_string(),
    };
    format!("{what}, {syscall} '{path}'")
}

/// How a child process ended: Node's `status ?? (signal ? 128 : 1)`, or None when it couldn't start.
fn exit_code(status: std::process::ExitStatus) -> u8 {
    match status.code() {
        Some(code) => code as u8,
        None => 128,
    }
}

impl Cli {
    fn main(&self, argv: &[String]) -> Result<u8, String> {
        let cmd = argv.first().map(String::as_str);
        let rest = argv.get(1..).unwrap_or(&[]);
        let arg = |i: usize| rest.get(i).map(String::as_str);
        match cmd {
            Some("--version" | "-v") => {
                out(&format!("{VERSION}\n"));
                return Ok(0);
            }
            Some("--") => return Ok(self.run_command(rest)),
            Some("shadow-report") => {
                shadow_report(arg(0).unwrap_or(""), arg(1).unwrap_or(""), &self.env, &log);
                return Ok(0);
            }
            Some("explain") => return self.explain(),
            Some("decide") => return Ok(self.decide_once()),
            Some("init") => return self.init(rest),
            Some("hook-exec") => {
                let script = arg(0).ok_or("The \"path\" argument must be of type string. Received undefined")?;
                return Ok(self.hook_exec(script, &rest[1..]));
            }
            Some("rewrite") => return self.rewrite(rest),
            _ => {}
        }
        if let Some(shell) = Shell::parse(cmd)
            && rest.len() == 1
        {
            return self.shell_exec(Some(shell), &rest[0]);
        }
        if let Some(file) = cmd
            && rest.is_empty()
            && npath::is_absolute(file)
            && exists(file)
        {
            return self.shell_exec(None, file);
        }
        log(USAGE);
        Ok(if matches!(cmd, None | Some("--help" | "-h")) { 0 } else { 2 })
    }

    fn options(&self, root: &str) -> DecideOptions {
        DecideOptions { root: root.to_string(), env: self.env.clone(), fetch: None }
    }

    fn decision_for(&self, cwd: &str) -> Result<(String, Decision), String> {
        let root = repo_root(cwd)?;
        let cached = decide_cached(&self.options(&root));
        if cached.report {
            if let Some(mismatch) = &cached.mismatch {
                log(&format!("picky: not reusing a shared decision: {mismatch}"));
            }
            if let Some(job) = cached.decision.job.as_deref().filter(|j| !j.is_empty())
                && self.env.get("GITHUB_JOB").map(String::as_str) != Some(job)
            {
                log(&format!("picky: using the decision computed by job \"{job}\""));
            }
            log(&format_decision(&cached.decision));
            write_job_summary(&cached.decision, &self.env);
        }
        Ok((root, cached.decision))
    }

    fn outcome_for(&self, argv: &[String]) -> Outcome {
        let cwd = cwd();
        let planned = self.decision_for(&cwd).and_then(|(root, decision)| plan_command(argv, &cwd, &root, &decision));
        planned.unwrap_or_else(|msg| Outcome::Full { note: format!("error, running in full: {msg}"), enforce: None })
    }

    fn explain(&self) -> Result<u8, String> {
        let root = repo_root(&cwd())?;
        let cached = decide_cached(&self.options(&root));
        log(&format_decision(&cached.decision));
        Ok(0)
    }

    /// `picky decide`: computes the decision into the runner's temp directory. A workflow uploads that
    /// directory as an artifact and every other job downloads it there, so one model call serves the PR.
    /// Always exits 0: a failure here must not block the jobs that need it; they decide on their own.
    fn decide_once(&self) -> u8 {
        match repo_root(&cwd()) {
            Ok(root) => {
                let cached = decide_cached(&self.options(&root));
                log(&format_decision(&cached.decision));
                write_job_summary(&cached.decision, &self.env);
                if let Some(file) = &cached.file {
                    log(&format!("picky: decision saved to {file}"));
                }
            }
            Err(msg) => log(&format!("picky: no shared decision ({msg}); each job decides on its own")),
        }
        0
    }

    /// `picky init`: prints the changes as a diff; `--write` applies them.
    fn init(&self, args: &[String]) -> Result<u8, String> {
        let write = args.iter().any(|a| a == "--write");
        let dist = match args.iter().position(|a| a == "--dist") {
            Some(i) => Some(args.get(i + 1).ok_or("--dist needs a directory")?.clone()),
            None => self.env.get("PICKY_DIST").filter(|d| !d.is_empty()).cloned(),
        };
        let root = repo_root(&cwd())?;
        let dist = dist.map(|d| npath::resolve(&[&cwd(), &d]));
        let mut binaries = ReleaseBinaries::new(dist, Box::new(log));
        let plan = plan_init(&root, &mut binaries)?;
        let files = &plan.files;

        if !write {
            for f in files {
                out(&format!("{}\n", diff_of(f)?));
            }
        } else {
            for f in files {
                let full = npath::join(&[&root, &f.path]);
                let Some(after) = &f.after else {
                    std::fs::remove_file(&full).map_err(|e| fs_error(&e, "unlink", &full))?;
                    continue;
                };
                let dir = npath::dirname(&full);
                std::fs::create_dir_all(&dir).map_err(|e| fs_error(&e, "mkdir", &dir))?;
                std::fs::write(&full, after.bytes()).map_err(|e| fs_error(&e, "open", &full))?;
                #[cfg(unix)]
                if let Some(mode) = f.mode {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(mode)).map_err(|e| fs_error(&e, "chmod", &full))?;
                }
            }
        }
        for n in &plan.notes {
            out(&format!("picky: {n}\n"));
        }
        if files.is_empty() {
            out("picky: already set up; nothing to change\n");
            return Ok(0);
        }
        let n = files.len();
        if !write {
            let s = if n == 1 { "" } else { "s" };
            out(&format!("picky: {n} file{s} would change; run `picky init --write` to apply\n"));
            return Ok(0);
        }
        out(&format!("picky: wrote {}\n", files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(", ")));
        let shadow = if files.iter().any(|f| f.path == ".picky.yml") {
            " picky starts in shadow mode: it logs what it would skip and runs everything."
        } else {
            ""
        };
        out(&format!("picky: next, add the PICKY_API_KEY secret, then commit.{shadow}\n"));
        Ok(0)
    }

    /// `picky -- cmd args`: the prefix form for CIs without a step hook.
    fn run_command(&self, argv: &[String]) -> u8 {
        if argv.is_empty() {
            log(USAGE);
            return 2;
        }
        let outcome = self.outcome_for(argv);
        log(&format!("picky: {}", outcome.note()));
        let run = match &outcome {
            Outcome::Skip { .. } => return 0,
            Outcome::Narrow { argv: narrowed, .. } => {
                log(&format!("picky: $ {}", quote_argv(narrowed)));
                narrowed.as_slice()
            }
            Outcome::Full { .. } => argv,
        };
        let res = self.spawn_argv(run);
        if res.failed
            && let Outcome::Full { enforce: Some(enforce), .. } = &outcome
        {
            self.shadow_check(argv, enforce);
        }
        res.code
    }

    /// `failed`: the command ran and exited non-zero (not: it could not start, or a signal stopped it).
    fn spawn_argv(&self, argv: &[String]) -> Spawned {
        let split = split_assignments(argv);
        let Some((program, args)) = split.rest.split_first() else {
            log("picky: The \"file\" argument must be of type string. Received undefined");
            return Spawned { code: 1, failed: false };
        };
        match Command::new(program).args(args).envs(&split.env).status() {
            Err(e) => {
                log(&format!("picky: spawnSync {program} {}", spawn_error_code(&e)));
                Spawned { code: 127, failed: false }
            }
            Ok(status) => match status.code() {
                Some(code) => Spawned { code: code as u8, failed: code != 0 },
                None => Spawned { code: 1, failed: false },
            },
        }
    }

    /// Shadow mode, after the full run failed: would enforce mode have let the failure through?
    fn shadow_check(&self, argv: &[String], enforce: &Enforce) {
        let report = |kind: &str, arg: &str| shadow_report(kind, arg, &self.env, &log);
        match enforce {
            Enforce::Skip { .. } => report("skipped", &shadow_miss_message(argv, enforce)),
            Enforce::Narrow { argv: narrowed, .. } => {
                report("check", &quote_argv(narrowed));
                if self.spawn_argv(narrowed).code != 0 {
                    report("caught", "");
                } else {
                    report("missed", &shadow_miss_message(argv, enforce));
                }
            }
        }
    }

    /// Called by the bash hook for a step script that contains `# picky` markers.
    fn hook_exec(&self, script: &str, args: &[String]) -> u8 {
        let env = |k: &str| self.env.get(k).map(String::as_str);
        let flags = bash_flags(env("PICKY_SHELL_FLAGS"), env("PICKY_PIPEFAIL") == Some("1"));
        let mut target = script.to_string();
        let prepared = read_text(script).and_then(|text| {
            let rewritten = rewrite_script(&text, &npath::resolve(&[script]), &self.self_bin(), self.marked_in_workflow(&text));
            // Nothing to decide in this step (its workflow is marked elsewhere): run it as is, keeping $0.
            if rewritten != text {
                let file = format!("{script}.picky.sh");
                std::fs::write(&file, rewritten).map_err(|e| fs_error(&e, "open", &file))?;
                target = file;
            }
            Ok(())
        });
        if let Err(msg) = prepared {
            log(&format!("picky: could not prepare the step ({msg}); running it unchanged"));
        }
        match Command::new("bash").args(&flags).arg(&target).args(args).status() {
            Err(e) => {
                log(&format!("picky: spawnSync bash {}", spawn_error_code(&e)));
                127
            }
            Ok(status) => exit_code(status),
        }
    }

    /// The picky the hook runs.
    fn self_bin(&self) -> String {
        match self.env.get("PICKY_BIN").filter(|b| !b.is_empty()) {
            Some(bin) => npath::resolve(&[bin]),
            None => self_exe(),
        }
    }

    /// `shell: picky [shell] {0}`: decides every command of the step, then runs it in the real shell.
    fn shell_exec(&self, named: Option<Shell>, file: &str) -> Result<u8, String> {
        let shell = named.unwrap_or_else(|| default_shell(cfg!(windows)));
        let text = read_text(file)?;
        let prepared = repo_root(&cwd()).and_then(|root| {
            let mut planner = CliPlanner { cli: self, root, text: &text };
            rewrite_step(&text, shell, &mut planner, &log)
        });
        let script = prepared.unwrap_or_else(|msg| {
            log(&format!("picky: could not prepare the step ({msg}); running it unchanged"));
            text.clone()
        });
        let target = format!("{file}.picky{}", extension(shell));
        std::fs::write(&target, finalize(&script, shell)).map_err(|e| fs_error(&e, "open", &target))?;
        let (cmd, args) = invocation(shell, &target);
        // The step's own bash must not hand it to picky a second time through the hook.
        match Command::new(&cmd).args(&args).env("PICKY_HOOK_ACTIVE", "1").status() {
            Err(e) => {
                log(&format!("picky: spawnSync {cmd} {}", spawn_error_code(&e)));
                Ok(127)
            }
            Ok(status) => Ok(exit_code(status)),
        }
    }

    /// True when the workflow marks this step with `run: cmd  # picky`, a YAML comment the script never sees.
    fn marked_in_workflow(&self, script: &str) -> bool {
        let Ok(root) = repo_root(&cwd()) else { return false };
        match load_job(&root, &self.env) {
            Ok(Ok(job)) => current_step(&job, script).is_some_and(|s| s.marked_in_yaml),
            _ => false,
        }
    }

    fn plan_setup(&self, text: &str) -> Result<JobPlan, String> {
        let root = repo_root(&cwd())?;
        plan_setup(text, &root, &self.env, || self.decision_for(&cwd()).map(|(_, d)| d))
    }

    /// Prints the shell text to run for one marked line. Always prints something runnable.
    fn rewrite(&self, args: &[String]) -> Result<u8, String> {
        let after = |flag: &str| {
            let i = args.iter().position(|a| a == flag).map_or(0, |i| i + 1);
            args.get(i).map(String::as_str)
        };
        let script = after("--script").ok_or("The \"path\" argument must be of type string. Received undefined")?;
        let line = parse_number(after("--line").unwrap_or("undefined"));
        let text = read_text(script)?;
        let source: Vec<&str> = text.split('\n').collect();
        let index = (line.fract() == 0.0 && line >= 1.0 && line <= source.len() as f64).then(|| line as usize - 1);
        let original = trim(index.map(|i| source[i]).ok_or("Cannot read properties of undefined (reading 'trim')")?);
        let words = tokenize(original);

        let marked = scan_script(&text, args.iter().any(|a| a == "--all")).into_iter().find(|l| Some(l.line - 1) == index);
        if !marked.is_some_and(|l| l.marked) {
            let (skip, note) = match self.plan_setup(&text) {
                Ok(plan) => (plan.skip, plan.note),
                Err(msg) => (false, format!("setup runs (error: {msg})")),
            };
            log(&format!("picky: {original} -> {note}"));
            out(if skip { ": skipped by picky\n" } else { "" });
            if !skip {
                out(&format!("{original}\n"));
            }
            return Ok(0);
        }

        let outcome = match words.as_deref() {
            Some(w) if !w.is_empty() => self.outcome_for(w),
            _ => Outcome::Full { note: "not a simple command; runs in full".into(), enforce: None },
        };
        log(&format!("picky: {} -> {}", label_of(original), outcome.note()));
        match &outcome {
            Outcome::Skip { .. } => out(": skipped by picky\n"),
            Outcome::Narrow { argv, .. } => {
                log(&format!("picky: $ {}", quote_argv(argv)));
                out(&format!("{}\n", quote_argv(argv)));
            }
            Outcome::Full { enforce: Some(enforce), .. } => {
                let words = words.unwrap_or_default();
                out(&format!("{}\n", shadow_line(Shell::Bash, &words, enforce, &[self.self_bin()])));
            }
            Outcome::Full { enforce: None, .. } => out(&format!("{original}\n")),
        }
        Ok(0)
    }
}

struct Spawned {
    code: u8,
    failed: bool,
}

/// This binary, for the commands picky's rewrites run.
fn self_exe() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "picky".into())
}

struct CliPlanner<'a> {
    cli: &'a Cli,
    root: String,
    text: &'a str,
}

impl StepPlanner for CliPlanner<'_> {
    fn line(&mut self, words: &[String]) -> Outcome {
        self.cli.outcome_for(words)
    }

    fn setup(&mut self) -> Result<JobPlan, String> {
        plan_setup(self.text, &self.root, &self.cli.env, || self.cli.decision_for(&cwd()).map(|(_, d)| d))
    }

    /// This process runs the shadow checks, wherever it is.
    fn self_command(&self) -> Vec<String> {
        vec![self_exe()]
    }
}

/// A unified diff of one planned file; binaries are summarized.
fn diff_of(f: &InitFile) -> Result<String, String> {
    let after = match &f.after {
        None => return Ok(format!("removed file {}", f.path)),
        Some(Content::Binary(bytes)) => {
            let what = if f.before.is_none() { "new" } else { "updated" };
            return Ok(format!("{what} file {} ({} MB)", f.path, to_fixed(bytes.len() as f64 / 1e6, 1)));
        }
        Some(Content::Text(text)) => text,
    };
    let tmp = make_temp_dir("picky-init-")?;
    let side = |name: &str, text: Option<&str>| -> Result<String, String> {
        let Some(text) = text else { return Ok("/dev/null".into()) };
        let file = npath::join(&[name, &f.path]);
        let full = npath::join(&[&tmp, &file]);
        std::fs::create_dir_all(npath::dirname(&full)).map_err(|e| e.to_string())?;
        std::fs::write(&full, text).map_err(|e| e.to_string())?;
        Ok(file)
    };
    let before = f.before.as_ref().map(|b| String::from_utf8_lossy(b).into_owned());
    let diff = side("a", before.as_deref()).and_then(|a| {
        let b = side("b", Some(after))?;
        let res = Command::new("git")
            .args(["diff", "--no-index", "--no-color", "--no-prefix", "--", &a, &b])
            .current_dir(&tmp)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("spawnSync git {}", spawn_error_code(&e)))?;
        Ok(trim_end(&String::from_utf8_lossy(&res.stdout)).to_string())
    });
    let _ = std::fs::remove_dir_all(&tmp);
    diff
}

/// `fs.mkdtempSync(path.join(os.tmpdir(), prefix))`
fn make_temp_dir(prefix: &str) -> Result<String, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for attempt in 0..100u32 {
        let suffix = format!("{:x}", (nanos as u64) ^ ((std::process::id() as u64) << 32) ^ attempt as u64);
        let dir = npath::join(&[&npath::tmpdir(), &format!("{prefix}{}", &suffix[suffix.len().saturating_sub(6)..])]);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(fs_error(&e, "mkdtemp", &dir)),
        }
    }
    Err("mkdtemp: no free name".into())
}
