use std::io::Write;

use crate::Env;
use crate::decide::Decision;
use crate::js;
use crate::plan::Enforce;
use crate::sh::quote_argv;

/// Human-readable decision for the terminal.
pub fn format_decision(d: &Decision) -> String {
    let mut lines = vec![format!(
        "picky: {} ({}, model {}, {} ms): {}",
        if d.full { "FULL RUN" } else { "selective" },
        d.mode.as_str(),
        d.model,
        d.ms,
        d.reason
    )];
    if let Some(base) = &d.base {
        lines.push(format!(
            "picky: diff {}..{}, {} changed files",
            prefix8(base),
            d.head.as_deref().map_or("undefined".to_string(), prefix8),
            d.changed.len()
        ));
    }
    lines.push(format!("picky: {}", model_line(d)));
    let mut tests: Vec<_> = d.tests.iter().collect();
    tests.sort_by(|a, b| js::locale_compare(a.0, b.0));
    for (test, t) in tests {
        let p = t.p.map_or(String::new(), |p| format!(" p={}", js::to_fixed(p, 2)));
        lines.push(format!("picky:   {} {test}  ({}{p})", if t.run { "RUN " } else { "skip" }, t.reason));
    }
    lines.join("\n")
}

/// `s.slice(0, 8)` in UTF-16 units.
fn prefix8(s: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().take(8).collect();
    String::from_utf16_lossy(&units)
}

fn model_line(d: &Decision) -> String {
    if let Some(u) = &d.model_usage {
        if let Some(error) = u.error.as_deref().filter(|e| !e.is_empty()) {
            return format!("model {} FAILED after {} ms ({error}); its {} tests run", u.model, u.ms, u.questions);
        }
        return format!(
            "model {} called: {} questions, {} request(s), {} input tokens, {} ms",
            u.model,
            u.questions,
            u.requests,
            js::number_to_string(u.input_tokens),
            u.ms
        );
    }
    if d.model == "none" {
        return "model not used (graph-only mode)".into();
    }
    if d.full {
        return "model not called (full run)".into();
    }
    "model not called (every test was decided by the graph or by rules)".into()
}

/// Appends the decision to the GitHub job summary, when running in Actions.
pub fn write_job_summary(d: &Decision, env: &Env) {
    let Some(file) = env.get("GITHUB_STEP_SUMMARY").filter(|f| !f.is_empty()) else { return };
    let mut tests: Vec<_> = d.tests.iter().collect();
    tests.sort_by(|a, b| js::locale_compare(a.0, b.0));
    let rows: Vec<String> = tests
        .into_iter()
        .map(|(test, t)| {
            let p = t.p.map_or(String::new(), |p| format!(" (p={})", js::to_fixed(p, 2)));
            format!("| {} | `{test}` | {}{p} |", if t.run { "run" } else { "skip" }, t.reason)
        })
        .collect();
    let mut md = vec![
        format!("### picky: {}", if d.full { "full run" } else { "selective" }),
        format!("{} · mode `{}` · {} · {} ms", d.reason, d.mode.as_str(), model_line(d), d.ms),
        String::new(),
    ];
    if !rows.is_empty() {
        md.push("| | Test file | Why |".into());
        md.push("|---|---|---|".into());
        md.extend(rows);
    }
    md.push(String::new());
    // the summary is informational only
    let _ = append(file, &md.join("\n"));
}

fn append(file: &str, text: &str) -> std::io::Result<()> {
    std::fs::OpenOptions::new().append(true).create(true).open(file)?.write_all(text.as_bytes())
}

/// What a shadow miss on this command says, written when the step is rewritten.
pub fn shadow_miss_message(argv: &[String], enforce: &Enforce) -> String {
    let full = quote_argv(argv);
    match enforce {
        Enforce::Skip { note } => {
            format!("enforce mode would have skipped `{full}` ({}), and it failed", note.strip_prefix("skip: ").unwrap_or(note))
        }
        Enforce::Narrow { argv: narrowed, note } => {
            format!("enforce mode would have run only `{}` ({note}), which passes, while the full `{full}` failed", quote_argv(narrowed))
        }
    }
}

/// The shadow check after a failed full run, in steps (`picky shadow-report <kind> [arg]`):
/// - `check <command>`: about to run what enforce mode would have run
/// - `caught`: that failed too
/// - `missed <message>`: that passed
/// - `skipped <message>`: enforce mode would have skipped the command
pub fn shadow_report(kind: &str, arg: &str, env: &Env, log: &dyn Fn(&str)) {
    let actions = env.get("GITHUB_ACTIONS").is_some_and(|v| v == "true");
    if kind == "check" {
        log("picky: shadow check: the full run failed; running what enforce mode would have run");
        log(&format!("picky: $ {arg}"));
        if actions {
            stdout("::group::picky shadow check\n");
        }
        return;
    }
    if actions && (kind == "caught" || kind == "missed") {
        stdout("::endgroup::\n");
    }
    if kind == "caught" {
        log("picky: shadow check: that fails too, so enforce mode would have caught this failure");
    } else {
        report_shadow_miss(arg, env, log);
    }
}

fn stdout(s: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// Shadow mode found a failure that enforce mode would have hidden: the signal shadow mode exists for.
/// Goes to the log, a warning annotation on the run, and the job summary.
fn report_shadow_miss(message: &str, env: &Env, log: &dyn Fn(&str)) {
    let advice = "If the failure is unrelated to the change (a flaky test, a runner problem), ignore it; otherwise enforce mode is not safe yet for this change.";
    log(&format!("picky: SHADOW MISS: {message}. {advice}"));
    if env.get("GITHUB_ACTIONS").is_some_and(|v| v == "true") {
        let escaped = message.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A");
        stdout(&format!("::warning title=picky shadow miss::{escaped}\n"));
    }
    let Some(file) = env.get("GITHUB_STEP_SUMMARY").filter(|f| !f.is_empty()) else { return };
    // the summary is informational only
    let _ = append(file, &format!("### picky: shadow miss\n{message}.\n\n{advice}\n\n"));
}
