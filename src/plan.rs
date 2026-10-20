use crate::commands::{CheckCommand, Command, Ecosystem, FilesCommand, TestCommand, ecosystem_of, recognize};
use crate::config::Mode;
use crate::decide::Decision;
use crate::git::exists;
use crate::glob::Glob;
use crate::graph::is_vendored;
use crate::npath;

/// What enforce mode would have done instead of a shadow-mode full run.
#[derive(Clone, Debug, PartialEq)]
pub enum Enforce {
    Skip { note: String },
    Narrow { argv: Vec<String>, note: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// `enforce`: in shadow mode, what enforce mode would have done instead of the full run.
    Full {
        note: String,
        enforce: Option<Enforce>,
    },
    Skip {
        note: String,
    },
    Narrow {
        argv: Vec<String>,
        note: String,
    },
}

impl Outcome {
    pub fn note(&self) -> &str {
        match self {
            Outcome::Full { note, .. } | Outcome::Skip { note } | Outcome::Narrow { note, .. } => note,
        }
    }

    fn full(note: impl Into<String>) -> Outcome {
        Outcome::Full { note: note.into(), enforce: None }
    }
}

const DOCS: &[&str] = &["**/*.{md,mdx,txt,rst,adoc}", "**/LICENSE*"];
const MEDIA: &[&str] = &["**/*.{png,jpg,jpeg,gif,webp,avif,svg,ico,bmp,woff,woff2,ttf,otf,eot,mp3,mp4,wav,webm,pdf}"];
const STYLES: &[&str] = &["**/*.{css,scss,sass,less,styl,pcss}"];
const JS: &[&str] = &[
    "**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs,vue,svelte,astro}",
    "**/{package,package-lock,tsconfig*,jsconfig}.json",
    "**/{pnpm-lock.yaml,yarn.lock,bun.lock,bun.lockb}",
];
const PHP: &[&str] = &["**/*.php", "**/composer.{json,lock}", "**/*.neon", "**/*.neon.dist", "**/phpunit*.xml*"];
const KOTLIN: &[&str] =
    &["**/*.{kt,kts,java}", "**/{gradle.properties,gradlew,gradlew.bat,pom.xml}", "**/gradle/wrapper/**", "**/gradle/*.versions.toml"];
const RUST: &[&str] = &["**/*.rs", "**/{Cargo.toml,Cargo.lock,rust-toolchain,rust-toolchain.toml}"];
const OTHER: &[&str] = &["**/*.{go,py,rb,swift,cs,c,h,cpp,hpp}", "**/{go.mod,go.sum}"];

/// Files that can't change a whole-project check's result. Anything not listed counts,
/// including unknown file types: a step may generate code from them before the check runs.
fn irrelevant(tool: &str) -> Option<Vec<&'static str>> {
    let groups: &[&[&str]] = match tool {
        "tsc" | "vue-tsc" | "svelte-check" => &[DOCS, MEDIA, STYLES, PHP, KOTLIN, RUST, OTHER],
        "biome" => &[MEDIA, PHP, KOTLIN, RUST, OTHER],
        "eslint" => &[MEDIA, STYLES, PHP, KOTLIN, RUST, OTHER],
        // Every other check of an ecosystem: phpstan, cargo clippy, gradle check, detekt, ...
        "php" => &[DOCS, MEDIA, STYLES, JS, KOTLIN, RUST, OTHER],
        "kotlin" => &[DOCS, MEDIA, STYLES, JS, PHP, RUST, OTHER],
        "rust" => &[DOCS, MEDIA, STYLES, JS, PHP, KOTLIN, OTHER],
        _ => return None,
    };
    Some(groups.concat())
}

/// Per-file tools that only take these files: anything else isn't passed to them.
fn only(tool: &str) -> Option<Vec<&'static str>> {
    (tool == "ktlint").then(|| vec!["**/*.{kt,kts}"])
}

/// Ecosystems whose checks also see files their sources read: a Rust `include_str!("../README.md")`,
/// or a resource a `gradle check` test loads. The graph links those files to their readers, so a
/// check counts an affected source, not only a changed one.
fn reads_files(e: Ecosystem) -> bool {
    matches!(e, Ecosystem::Kotlin | Ecosystem::Rust)
}

/// Per-file tools: a change to these (or to a file they import) can change every file's result.
fn tool_config(tool: &str) -> Option<Vec<&'static str>> {
    Some(match tool {
        "eslint" => vec!["**/eslint.config.*", "**/.eslintrc*", "**/.eslintignore"],
        "prettier" => vec!["**/.prettierrc*", "**/prettier.config.*", "**/.prettierignore", "**/.editorconfig"],
        "ktlint" => vec!["**/.editorconfig", "**/.ktlint*", "**/baseline.xml"],
        _ => return None,
    })
}

/// `picomatch(globs, { dot: true })` for picky's own glob lists, which are always valid.
fn matcher(globs: &[&str]) -> Glob {
    Glob::new(globs).expect("valid globs")
}

/// Decides what to do with one marked command, given the PR-wide decision.
pub fn plan_command(argv: &[String], cwd: &str, root: &str, decision: &Decision) -> Result<Outcome, String> {
    let Some(command) = recognize(argv, cwd)? else { return Ok(Outcome::full("command not recognized; runs in full")) };
    if decision.full {
        return Ok(Outcome::full(format!("full run: {}", decision.reason)));
    }
    let eco = command.ecosystem();
    if let Some(reason) = decision.ecosystems.as_ref().and_then(|e| e.get(eco.as_str())).filter(|r| !r.is_empty()) {
        return Ok(Outcome::full(format!("full run for {} commands: {reason}", eco.as_str())));
    }

    let outcome = match &command {
        Command::Tests(c) => plan_tests(c, cwd, root, decision),
        Command::Files(c) => plan_files(c, cwd, root, decision),
        Command::Check(c) => plan_check(c, decision),
    };
    if decision.mode == Mode::Shadow {
        let enforce = match &outcome {
            Outcome::Full { .. } => None,
            Outcome::Skip { note } => Some(Enforce::Skip { note: note.clone() }),
            Outcome::Narrow { argv, note } => Some(Enforce::Narrow { argv: argv.clone(), note: note.clone() }),
        };
        if let Some(enforce) = enforce {
            let note = outcome.note();
            let would = note.strip_prefix("running").map_or_else(|| note.to_string(), |rest| format!("run{rest}"));
            return Ok(Outcome::Full { note: format!("shadow mode: would {would}; running in full"), enforce: Some(enforce) });
        }
    }
    Ok(outcome)
}

fn plan_tests(command: &TestCommand, cwd: &str, root: &str, decision: &Decision) -> Outcome {
    // Only this runner's tests under the command's working directory belong to it.
    let here = npath::to_posix(&npath::relative(root, cwd));
    let rel = |t: &str| npath::to_posix(&npath::relative(cwd, &npath::join(&[root, t])));
    let tests: Vec<&String> = decision
        .tests
        .keys()
        .filter(|t| {
            (here.is_empty() || t.starts_with(&format!("{here}/")))
                && ecosystem_of(t) == command.ecosystem
                && command.owns.as_ref().is_none_or(|owns| owns(&rel(t)))
        })
        .collect();
    if tests.is_empty() {
        return Outcome::full("no test files found under this directory; runs in full");
    }

    let selected: Vec<&String> = tests.iter().copied().filter(|t| decision.tests[*t].run).collect();
    let summary = format!("{} of {} test files", selected.len(), tests.len());
    if selected.len() == tests.len() {
        return Outcome::full(format!("all {} test files affected", tests.len()));
    }
    if selected.is_empty() {
        return Outcome::Skip { note: format!("skip: no affected tests (0 of {})", tests.len()) };
    }

    let files: Vec<String> = selected.iter().map(|t| rel(t)).collect();
    match (command.narrow)(&files) {
        None => Outcome::full(format!(
            "{}; runs in full",
            command.why_not.clone().unwrap_or_else(|| format!("{} can't run only the selected tests", command.tool))
        )),
        Some(argv) => Outcome::Narrow { argv, note: format!("running {summary}") },
    }
}

fn plan_files(command: &FilesCommand, cwd: &str, root: &str, decision: &Decision) -> Outcome {
    let Some(affected) = &decision.affected else { return Outcome::full("no file list in this decision; runs in full") };
    let present: Vec<String> = decision.changed.iter().filter(|c| c.status != "D").map(|c| c.path.clone()).collect();
    let is_config = matcher(&tool_config(&command.tool).unwrap_or_else(|| vec!["\0"]));
    // decision.affected includes files that import a changed file, so a changed local rule or plugin counts too.
    let config = decision.changed.iter().map(|c| &c.path).chain(affected.iter()).find(|f| is_config.is_match(f));
    if let Some(config) = config {
        return Outcome::full(format!("{} config {config} affected; runs in full", command.tool));
    }

    let targets: Vec<String> =
        command.targets.iter().map(|t| npath::to_posix(&npath::relative(root, &npath::resolve(&[cwd, t])))).collect();
    let in_targets = |f: &str| targets.iter().any(|t| t.is_empty() || f == t || f.starts_with(&format!("{t}/")));
    let unchecked = matcher(&irrelevant(&command.tool).unwrap_or_else(|| vec!["\0"]));
    let checked = matcher(&only(&command.tool).unwrap_or_else(|| vec!["**"]));
    let pool = if command.with_dependents { affected } else { &present };
    let files: Vec<&String> = pool
        .iter()
        .filter(|f| {
            // The tools error on paths that don't exist, so only files on disk are passed.
            in_targets(f) && checked.is_match(f) && !unchecked.is_match(f) && !is_vendored(f) && exists(&npath::join(&[root, f]))
        })
        .collect();
    if files.is_empty() {
        return Outcome::Skip { note: format!("skip: no affected files for {}", command.tool) };
    }
    let rel: Vec<String> = files.iter().map(|f| npath::to_posix(&npath::relative(cwd, &npath::join(&[root, f])))).collect();
    Outcome::Narrow { argv: (command.narrow)(&rel), note: format!("running {} on {} affected files", command.tool, files.len()) }
}

fn plan_check(command: &CheckCommand, decision: &Decision) -> Outcome {
    let globs = irrelevant(&command.tool).or_else(|| irrelevant(command.ecosystem.as_str())).unwrap_or_else(|| vec!["\0"]);
    let irrelevant = matcher(&globs);
    let paths: Vec<&String> = decision.changed.iter().flat_map(|c| c.from.iter().chain(std::iter::once(&c.path))).collect();
    let relevant: Vec<&&String> = paths.iter().filter(|p| !irrelevant.is_match(p)).collect();
    if let Some(first) = relevant.first() {
        return Outcome::full(format!("{} changed files {} checks (e.g. {first}); runs in full", relevant.len(), command.tool));
    }
    let readers: Vec<&String> = if reads_files(command.ecosystem) {
        decision.affected.iter().flatten().filter(|p| !irrelevant.is_match(p)).collect()
    } else {
        Vec::new()
    };
    if let Some(first) = readers.first() {
        return Outcome::full(format!("{} files {} checks read a changed file (e.g. {first}); runs in full", readers.len(), command.tool));
    }
    Outcome::Skip { note: format!("skip: no changes {} checks", command.tool) }
}
