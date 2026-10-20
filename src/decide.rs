//! The decision: which test files a change can affect, from the dependency graph and, for the
//! tests the graph can't rule in, the model.

use std::sync::{Arc, LazyLock};

use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Env;
use crate::commands::{Ecosystem, ecosystem_of};
use crate::config::{Config, Mode, Provider, load_config};
use crate::git::{Change, Range, changed_files, git, hunks, resolve_range, tracked_files};
use crate::glob::Glob;
use crate::graph::{Graph, build_graph, importers_of_missing, is_vendored, jvm_referencers_of, php_referencers_of, transitive_dependents};
use crate::jsre::{JsRegex, from_units, units};
use crate::judge::{Fetch, Http, JudgeOptions, Question, ask_jev, now_ms};
use crate::kotlin::{included_builds, is_jvm_source, jvm_declared_names};
use crate::npath::{self, posix};
use crate::php::declared_names;
use crate::rust::crate_dir_of;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestDecision {
    pub run: bool,
    pub reason: String,
    /// Model probability that the change affects this test, when asked.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub p: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub version: u8,
    pub mode: Mode,
    pub model: String,
    /// True means every marked command runs unchanged.
    pub full: bool,
    pub reason: String,
    /// Ecosystems whose commands all run in full, with the reason (e.g. a lockfile changed).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ecosystems: Option<IndexMap<String, String>>,
    /// Changed files that still exist plus every file that transitively depends on a changed file.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub affected: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head: Option<String>,
    pub changed: Vec<Change>,
    pub tests: IndexMap<String, TestDecision>,
    /// Present when the model was called, so a run can prove it used the model.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model_usage: Option<ModelUsage>,
    /// The GitHub job that computed this decision; other jobs reuse it when it's shared as an artifact.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub job: Option<String>,
    /// What the cache key is made of, so a job that can't reuse a shared decision can say why.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub inputs: Option<IndexMap<String, String>>,
    pub ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub questions: usize,
    pub requests: usize,
    pub input_tokens: f64,
    pub ms: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

pub const THRESHOLD: f64 = 0.15;
const DEFAULT_BUDGET_SECONDS: f64 = 30.0;
const MAX_STATE_CHARS: usize = 200_000;
pub const TEST_GLOBS: &[&str] = &["**/*.{test,spec}.{ts,tsx,mts,cts,js,jsx,mjs,cjs}", "**/*Test.php"];
/// Changes here can alter anything: every marked command runs in full.
const FULL_RUN_GLOBS: &[&str] = &[".picky.yml", ".github/workflows/**", ".github/picky/**"];

/// Changes here can alter any result in one ecosystem: its commands run in full.
fn ecosystem_globs(eco: Ecosystem) -> &'static [&'static str] {
    match eco {
        Ecosystem::Js => &[
            "**/package.json",
            "**/package-lock.json",
            "**/pnpm-lock.yaml",
            "**/yarn.lock",
            "**/bun.lock",
            "**/bun.lockb",
            "**/tsconfig*.json",
            "**/jsconfig.json",
            "**/{vitest,vite,jest}.config.*",
        ],
        Ecosystem::Php => &["**/composer.json", "**/composer.lock", "**/phpunit.xml", "**/phpunit.xml.dist", "**/phpunit.dist.xml"],
        Ecosystem::Kotlin => &[
            "**/*.gradle",
            "**/*.gradle.kts",
            "**/gradle.properties",
            "**/gradle/wrapper/**",
            "**/gradle/*.versions.toml",
            "**/{gradlew,gradlew.bat}",
            "**/buildSrc/**",
            "**/pom.xml",
            "**/.mvn/**",
        ],
        Ecosystem::Rust => {
            &["**/Cargo.toml", "**/Cargo.lock", "**/rust-toolchain", "**/rust-toolchain.toml", "**/.cargo/config", "**/.cargo/config.toml"]
        }
    }
}

/// Bump when the decision's shape changes, so cached decisions from older versions aren't reused.
const DECISION_SCHEMA: u32 = 5;

#[derive(Clone)]
pub struct DecideOptions {
    pub root: String,
    pub env: Env,
    pub fetch: Option<Arc<dyn Fetch>>,
}

pub struct CachedDecision {
    pub decision: Decision,
    pub cached: bool,
    /// True the first time this job sees the decision: computed here, or shared by another job.
    pub report: bool,
    /// Where the decision is stored; None when it couldn't be keyed.
    pub file: Option<String>,
    /// Set when another job's decision is here but was made from different inputs.
    pub mismatch: Option<String>,
}

fn env_get<'a>(env: &'a Env, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str)
}

/// Computes the decision, reusing a cached one for the same inputs. Never fails.
/// The cache lives in the runner's temp directory, so a decision computed once (by `picky decide`)
/// and downloaded there as an artifact is reused by every job: they all act on the same model answers.
pub fn decide_cached(opts: &DecideOptions) -> CachedDecision {
    let mut file: Option<String> = None;
    let mut inputs: Option<IndexMap<String, String>> = None;
    let job = env_get(&opts.env, "GITHUB_JOB");
    let cached = (|| -> Result<Decision, String> {
        let config = load_config(&opts.root, &opts.env);
        let keyed = IndexMap::from([
            ("schema".to_string(), DECISION_SCHEMA.to_string()),
            ("head".to_string(), crate::js::trim(&git(&opts.root, &["rev-parse", "HEAD"])?).to_string()),
            ("config".to_string(), digest(config.source.as_bytes())),
            ("base".to_string(), env_get(&opts.env, "PICKY_BASE").unwrap_or("").to_string()),
            ("event".to_string(), env_get(&opts.env, "GITHUB_EVENT_NAME").unwrap_or("").to_string()),
            // The event's content, not its path: a container job sees the same event at another path.
            ("payload".to_string(), event_digest(&opts.env)),
        ]);
        let key = serde_json::to_string(&keyed).expect("serializable");
        inputs = Some(keyed);
        let path = npath::join(&[&cache_dir(&opts.root, &opts.env)?, &format!("decision-{}.json", &digest(key.as_bytes())[..16])]);
        file = Some(path.clone());
        let text = std::fs::read(&path).map_err(|e| e.to_string())?;
        serde_json::from_slice(&text).map_err(|e| e.to_string())
    })();
    if let Ok(decision) = cached {
        let file = file.expect("set before reading");
        let report = decision.job.as_deref() != job && first_sight(&file, job);
        return CachedDecision { decision, cached: true, report, file: Some(file), mismatch: None };
    }

    let mismatch = match (&file, &inputs) {
        (Some(f), Some(i)) => shared_mismatch(&npath::dirname(f), i),
        _ => None,
    };
    let mut decision = decide(opts);
    if let Some(job) = job.filter(|j| !j.is_empty()) {
        decision.job = Some(job.to_string());
    }
    if inputs.is_some() {
        decision.inputs = inputs;
    }
    if let Some(f) = &file {
        // caching is an optimization only
        let _ = std::fs::create_dir_all(npath::dirname(f))
            .and_then(|_| std::fs::write(f, serde_json::to_string_pretty(&decision).expect("serializable")));
    }
    CachedDecision { decision, cached: false, report: true, file, mismatch }
}

static DECISION_FILE: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"^decision-\w+\.json$", ""));

/// Names the inputs that differ from a decision already in the cache directory, e.g. one downloaded from the `picky` job.
fn shared_mismatch(dir: &str, inputs: &IndexMap<String, String>) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|f| DECISION_FILE.test(&units(f)))
        .collect();
    names.sort(); // Node's readdirSync lists entries sorted by name
    for name in names {
        let other = crate::json::read(&npath::join(&[dir, &name]))?;
        if matches!(other, serde_json::Value::Null) {
            return None; // `null.inputs` throws
        }
        let other_inputs = crate::json::get(&other, "inputs");
        if !crate::json::truthy(other_inputs) {
            continue;
        }
        let other_inputs = other_inputs.expect("truthy");
        let differ: Vec<&str> = inputs
            .iter()
            .filter(|(k, v)| crate::json::get(other_inputs, k).and_then(serde_json::Value::as_str) != Some(v.as_str()))
            .map(|(k, _)| k.as_str())
            .collect();
        let job = match crate::json::get(&other, "job") {
            None | Some(serde_json::Value::Null) => "unknown".to_string(),
            Some(v) => crate::json::to_js_string(v),
        };
        return Some(format!("the decision from job \"{job}\" was made from a different {}", differ.join(", ")));
    }
    None
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The event payload, parsed: Windows writes the same event with other line endings and spacing.
fn event_digest(env: &Env) -> String {
    let Some(path) = env_get(env, "GITHUB_EVENT_PATH").filter(|p| !p.is_empty()) else { return String::new() };
    let Ok(bytes) = std::fs::read(path) else { return String::new() };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    match crate::json::parse(text) {
        Ok(v) => digest(serde_json::to_string(&v).expect("serializable").as_bytes()),
        Err(_) => String::new(),
    }
}

/// Claims the report of a shared decision for this job, so only its first marked command prints it.
fn first_sight(file: &str, job: Option<&str>) -> bool {
    std::fs::OpenOptions::new().write(true).create_new(true).open(format!("{file}.seen-{}", job.unwrap_or("local"))).is_ok()
}

fn cache_dir(root: &str, env: &Env) -> Result<String, String> {
    if let Some(temp) = env_get(env, "RUNNER_TEMP").filter(|t| !t.is_empty()) {
        return Ok(npath::join(&[temp, "picky"]));
    }
    let git_path = git(root, &["rev-parse", "--git-path", "picky"])?;
    Ok(npath::resolve(&[root, crate::js::trim(&git_path)]))
}

/// Computes a fresh decision. Any internal error becomes a full run.
pub fn decide(opts: &DecideOptions) -> Decision {
    let started = now_ms();
    let config = load_config(&opts.root, &opts.env);
    match decide_inner(opts, &config, started) {
        Ok(d) => d,
        Err(message) => full_run(Some(&config), &format!("internal error: {message}"), Vec::new(), started),
    }
}

fn glob(globs: &[impl AsRef<str>]) -> Result<Glob, String> {
    Glob::new(globs)
}

/// `picomatch(list.length ? list : ['\0'], { dot: true })`
fn glob_or_none(globs: &[String]) -> Result<Glob, String> {
    if globs.is_empty() { glob(&["\0"]) } else { glob(globs) }
}

fn decide_inner(opts: &DecideOptions, config: &Config, started: f64) -> Result<Decision, String> {
    let (root, env) = (opts.root.as_str(), &opts.env);
    if let Some(problem) = &config.problem {
        return Ok(full_run(Some(config), problem, Vec::new(), started));
    }

    let event = if env_get(env, "GITHUB_ACTIONS") == Some("true") { env_get(env, "GITHUB_EVENT_NAME") } else { None };
    if let Some(event) = event.filter(|e| !e.is_empty() && *e != "pull_request") {
        return Ok(full_run(Some(config), &format!("event \"{event}\" always runs everything"), Vec::new(), started));
    }
    if config.model.provider == Provider::Typesafe && config.api_key.as_deref().is_none_or(str::is_empty) {
        return Ok(full_run(Some(config), "no API key (set PICKY_API_KEY)", Vec::new(), started));
    }

    let Some(range) = resolve_range(root, env) else {
        return Ok(full_run(Some(config), "no base commit to compare against (set PICKY_BASE)", Vec::new(), started));
    };

    let changed = changed_files(root, &range)?;
    let with_range = |mut d: Decision| {
        d.base = Some(range.base.clone());
        d.head = Some(range.head.clone());
        d
    };

    let touches = |globs: &[String]| -> Result<Option<&Change>, String> {
        let m = glob(globs)?;
        Ok(changed.iter().find(|c| m.is_match(&c.path) || c.from.as_ref().is_some_and(|f| m.is_match(f))))
    };
    let owned = |globs: &[&str]| globs.iter().map(|g| g.to_string()).collect::<Vec<_>>();
    if let Some(trigger) = touches(&owned(FULL_RUN_GLOBS))? {
        return Ok(with_range(full_run(Some(config), &format!("{} changed", trigger.path), changed.clone(), started)));
    }

    let is_critical = glob_or_none(&config.critical)?;
    if let Some(c) = changed.iter().find(|c| is_critical.is_match(&c.path)) {
        let reason = format!("critical path {} changed", c.path);
        return Ok(with_range(full_run(Some(config), &reason, changed.clone(), started)));
    }

    let files = tracked_files(root)?;
    // Gradle build logic (`includeBuild` in a settings file) configures every project, like buildSrc.
    let build_logic: Vec<String> = included_builds(root, &files).into_iter().map(|dir| format!("{dir}/**")).collect();
    let mut ecosystems: IndexMap<String, String> = IndexMap::new();
    for eco in Ecosystem::ALL {
        let mut globs = owned(ecosystem_globs(eco));
        if eco == Ecosystem::Kotlin {
            globs.extend(build_logic.iter().cloned());
        }
        if let Some(hit) = touches(&globs)? {
            ecosystems.insert(eco.as_str().to_string(), format!("{} changed", hit.path));
        }
    }

    let is_test = glob(TEST_GLOBS)?;
    let graph = build_graph(root, &files)?;
    let tests: Vec<String> =
        files.iter().filter(|f| is_test.is_match(f) && !is_vendored(f)).cloned().chain(graph.test_units.iter().cloned()).collect();

    let is_low = glob_or_none(&config.low)?;
    // A low-risk file that code reads (a README a Rust crate includes as its doctests) still counts.
    let read_by_code =
        |f: &str| graph.dependents.get(f).is_some_and(|d| !d.is_empty()) || !importers_of_missing(&graph, &[f.to_string()]).is_empty();
    let relevant: Vec<Change> = changed
        .iter()
        .filter(|c| !is_low.is_match(&c.path) || read_by_code(&c.path) || c.from.as_deref().is_some_and(read_by_code))
        .cloned()
        .collect();
    let present: Vec<String> = relevant.iter().filter(|c| c.status != "D").map(|c| c.path.clone()).collect();
    let missing: Vec<String> = relevant
        .iter()
        .filter_map(|c| if c.status == "D" { Some(c.path.clone()) } else { c.from.clone().filter(|f| !f.is_empty()) })
        .collect();

    // Floor: changed tests, tests importing changed files, tests importing removed files.
    let mut seeds: IndexSet<String> = present.iter().cloned().collect();
    seeds.extend(importers_of_missing(&graph, &missing));
    seeds.extend(users_of_removed_php(root, &range, &graph, &missing));
    seeds.extend(users_of_removed_jvm(root, &range, &graph, &missing));
    seeds.extend(crates_of_removed(&graph, &missing));
    let mut affected = seeds.clone();
    affected.extend(transitive_dependents(&graph, &seeds));

    let mut decisions: IndexMap<String, TestDecision> = IndexMap::new();
    let mut candidates: Vec<String> = Vec::new();
    let mut model_usage: Option<ModelUsage> = None;
    let decided = |run: bool, reason: String| TestDecision { run, reason, p: None };
    for test in &tests {
        if let Some(reason) = ecosystems.get(ecosystem_of(test).as_str()) {
            decisions.insert(test.clone(), decided(true, reason.clone()));
        } else if is_critical.is_match(test) {
            decisions.insert(test.clone(), decided(true, "critical test".into()));
        } else if present.contains(test) {
            decisions.insert(test.clone(), decided(true, "test changed".into()));
        } else if affected.contains(test) {
            decisions.insert(test.clone(), decided(true, format!("imports {}", first_changed_import(&graph, test, &seeds))));
        } else {
            candidates.push(test.clone());
        }
    }

    if relevant.is_empty() {
        for test in &candidates {
            decisions.insert(test.clone(), decided(false, "only low-risk files changed".into()));
        }
    } else if !candidates.is_empty() {
        let ctx = JudgeContext { root, config, range: &range, relevant: &relevant, graph: &graph, opts, started };
        model_usage = judge_candidates(&candidates, &mut decisions, &ctx)?;
    }

    let selected = decisions.values().filter(|d| d.run).count();
    let mut all_affected: Vec<String> = changed
        .iter()
        .filter(|c| c.status != "D")
        .map(|c| c.path.clone())
        .chain(affected.iter().cloned())
        .collect::<IndexSet<_>>()
        .into_iter()
        .collect();
    crate::js::sort_strings(&mut all_affected);
    Ok(Decision {
        version: 1,
        mode: config.mode,
        model: config.model.id.clone(),
        full: false,
        reason: format!("selected {selected} of {} test files", tests.len()),
        ecosystems: (!ecosystems.is_empty()).then_some(ecosystems),
        // Every existing changed file (low-risk ones too: formatters check them) plus dependents.
        affected: Some(all_affected),
        base: Some(range.base.clone()),
        head: Some(range.head.clone()),
        changed,
        tests: decisions,
        model_usage,
        job: None,
        inputs: None,
        ms: elapsed(started),
    })
}

fn elapsed(since: f64) -> u64 {
    (now_ms() - since).max(0.0) as u64
}

struct JudgeContext<'a> {
    root: &'a str,
    config: &'a Config,
    range: &'a Range,
    relevant: &'a [Change],
    graph: &'a Graph,
    opts: &'a DecideOptions,
    started: f64,
}

fn judge_candidates(
    candidates: &[String],
    decisions: &mut IndexMap<String, TestDecision>,
    ctx: &JudgeContext,
) -> Result<Option<ModelUsage>, String> {
    let run_all = |decisions: &mut IndexMap<String, TestDecision>, reason: &str| {
        for test in candidates {
            decisions.insert(test.clone(), TestDecision { run: true, reason: reason.to_string(), p: None });
        }
    };

    if ctx.config.model.provider == Provider::None {
        for test in candidates {
            let reason = "no graph link (graph-only mode, model: none)".to_string();
            decisions.insert(test.clone(), TestDecision { run: false, reason, p: None });
        }
        return Ok(None);
    }

    let state = build_state(ctx)?;
    let state_len = state.encode_utf16().count();
    // Unseen means run: never let the model decide on a diff it can't fully see.
    if state_len > MAX_STATE_CHARS {
        let reason = format!("diff too large for one request ({state_len} chars); split context lands in a later version");
        run_all(decisions, &reason);
        return Ok(None);
    }

    let questions: Vec<Question> =
        candidates.iter().enumerate().map(|(i, test)| Question { id: format!("t{i}"), instructions: question_for(ctx, test) }).collect();
    let budget = env_get(&ctx.opts.env, "PICKY_BUDGET_SECONDS").map_or(DEFAULT_BUDGET_SECONDS, crate::js::parse_number) * 1000.0;
    let mut usage =
        ModelUsage { model: ctx.config.model.name.clone(), questions: questions.len(), requests: 0, input_tokens: 0.0, ms: 0, error: None };
    let asked = now_ms();
    let options = JudgeOptions {
        api_key: ctx.config.api_key.clone().unwrap_or_default(),
        model: ctx.config.model.name.clone(),
        deadline: ctx.started + budget,
        fetch: ctx.opts.fetch.clone().unwrap_or_else(|| Arc::new(Http)),
        endpoint: env_get(&ctx.opts.env, "PICKY_JEV_ENDPOINT").map(str::to_string),
        batch_size: 50,
    };
    let answers = match ask_jev(&state, &questions, &options) {
        Ok(result) => {
            usage.requests = result.requests;
            usage.input_tokens = result.input_tokens;
            usage.ms = elapsed(asked);
            result.answers
        }
        Err(message) => {
            run_all(decisions, &format!("model unavailable ({message})"));
            return Ok(Some(ModelUsage { ms: elapsed(asked), error: Some(message), ..usage }));
        }
    };

    for (i, test) in candidates.iter().enumerate() {
        let decision = match answers.get(&format!("t{i}")) {
            None => TestDecision { run: true, reason: "model gave no answer".into(), p: None },
            Some(&p) if p >= THRESHOLD => TestDecision { run: true, reason: "model: possibly affected".into(), p: Some(p) },
            Some(&p) => TestDecision { run: false, reason: "model: unaffected".into(), p: Some(p) },
        };
        decisions.insert(test.clone(), decision);
    }
    Ok(Some(usage))
}

fn build_state(ctx: &JudgeContext) -> Result<String, String> {
    let overview = ctx.relevant.iter().map(|c| {
        let name = match c.from.as_deref().filter(|f| !f.is_empty()) {
            Some(from) => format!("{from} -> {}", c.path),
            None => c.path.clone(),
        };
        format!("{} {name} (+{} -{})", c.status, c.added, c.removed)
    });
    let files: Vec<String> = ctx
        .relevant
        .iter()
        .flat_map(|c| match c.from.as_deref().filter(|f| !f.is_empty()) {
            Some(from) => vec![from.to_string(), c.path.clone()],
            None => vec![c.path.clone()],
        })
        .collect();
    let diff = hunks(ctx.root, ctx.range, &files)?;
    let mut lines = vec![format!("Code change under review: {} files.", ctx.relevant.len())];
    lines.extend(overview);
    lines.push(String::new());
    lines.push("Diff (zero context lines):".into());
    lines.push(diff);
    Ok(lines.join("\n"))
}

fn question_for(ctx: &JudgeContext, test: &str) -> String {
    let is_crate = posix::basename(test, None) == "Cargo.toml";
    let all_imports: Vec<&String> = ctx.graph.imports.get(test).map(|s| s.iter().collect()).unwrap_or_default();
    let imports: Vec<&str> = all_imports.iter().take(10).map(|s| s.as_str()).collect();
    // A crate's manifest stands for the tests inside its sources: unit tests and doctests.
    let sources: Vec<&str> =
        if is_crate { all_imports.iter().filter(|f| f.ends_with(".rs")).map(|s| s.as_str()).collect() } else { vec![test] };
    let names: Vec<String> = sources.iter().flat_map(|f| test_names(&npath::join(&[ctx.root, f]))).take(12).collect();
    let subject = if is_crate {
        format!("the unit tests and doctests of the Rust crate in \"{}\"", posix::dirname(test))
    } else {
        format!("the tests in \"{test}\"")
    };
    let parts = [
        format!("Could the code change in the state alter the result of {subject}?"),
        if names.is_empty() { String::new() } else { format!("Test names: {}.", names.join("; ")) },
        if imports.is_empty() { "It imports no changed project files.".into() } else { format!("It imports: {}.", imports.join(", ")) },
    ];
    parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Files that use a class or function declared in a removed or renamed-away PHP file.
fn users_of_removed_php(root: &str, range: &Range, graph: &Graph, missing: &[String]) -> IndexSet<String> {
    let names: Vec<String> = missing
        .iter()
        .filter(|f| f.ends_with(".php"))
        .flat_map(|f| git(root, &["show", &format!("{}:{f}", range.base)]).map(|t| declared_names(&t)).unwrap_or_default())
        .collect();
    if names.is_empty() { IndexSet::new() } else { php_referencers_of(graph, &names) }
}

/// Files that use a class or function declared in a removed or renamed-away Kotlin or Java file.
fn users_of_removed_jvm(root: &str, range: &Range, graph: &Graph, missing: &[String]) -> IndexSet<String> {
    let names: Vec<String> = missing
        .iter()
        .filter(|f| is_jvm_source(f))
        .flat_map(|f| git(root, &["show", &format!("{}:{f}", range.base)]).map(|t| jvm_declared_names(f, &t)).unwrap_or_default())
        .collect();
    if names.is_empty() { IndexSet::new() } else { jvm_referencers_of(graph, &names) }
}

/// The crate of each removed Rust file: its `mod` declaration, or an `include!` of it, no longer compiles.
fn crates_of_removed(graph: &Graph, missing: &[String]) -> Vec<String> {
    let dirs: IndexSet<String> = graph.rust_crates.iter().cloned().collect();
    missing.iter().filter_map(|f| crate_dir_of(&dirs, f)).map(|dir| posix::join(&[&dir, "Cargo.toml"])).collect()
}

static CALLS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r#"\b(?:describe|it|test)\s*\(\s*(['"`])((?:\\.|(?!\1).)*)\1"#, "g"));
static PHP_TESTS: LazyLock<JsRegex> = LazyLock::new(|| JsRegex::new(r"\bfunction\s+(test\w+)\s*\(", "g"));
static RUST_TESTS: LazyLock<JsRegex> =
    LazyLock::new(|| JsRegex::new(r"#\[(?:\w+::)*test\b[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)", "g"));
static JVM_TESTS: LazyLock<JsRegex> = LazyLock::new(|| {
    JsRegex::new(
        r"@(?:Test|ParameterizedTest|RepeatedTest)\b(?:\([^)]*\))?\s+(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|internal|protected|open|override|suspend)\s+)*(?:fun\s+(?:`([^`]+)`|(\w+))|void\s+(\w+))",
        "g",
    )
});

fn test_names(file: &str) -> Vec<String> {
    let Some(text) = crate::kotlin::read(file) else { return Vec::new() };
    let u = units(&text);
    let real = |s: &str| from_units(s).into_owned();
    let mut out: Vec<String> = CALLS.match_all(&u).iter().map(|m| real(m.str(2))).collect();
    // PHPUnit: test methods. Rust: #[test] (and #[tokio::test]) functions. JUnit: @Test functions and methods.
    out.extend(PHP_TESTS.match_all(&u).iter().map(|m| real(m.str(1))));
    out.extend(RUST_TESTS.match_all(&u).iter().map(|m| real(m.str(1))));
    out.extend(JVM_TESTS.match_all(&u).iter().map(|m| real(m.get(1).or(m.get(2)).or(m.get(3)).unwrap_or(""))));
    out
}

fn first_changed_import(graph: &Graph, test: &str, seeds: &IndexSet<String>) -> String {
    // Breadth-first from the test to the nearest changed file, for a readable reason.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::from([test]);
    let mut queue: std::collections::VecDeque<&str> = std::collections::VecDeque::from([test]);
    while let Some(file) = queue.pop_front() {
        for dep in graph.imports.get(file).into_iter().flatten() {
            if seeds.contains(dep) {
                return dep.clone();
            }
            if seen.insert(dep) {
                queue.push_back(dep);
            }
        }
    }
    for (file, targets) in &graph.unresolved {
        if file == test || seen.contains(file.as_str()) {
            return format!("{} (removed)", targets.first().map_or("undefined", String::as_str));
        }
    }
    "a changed file".into()
}

fn full_run(config: Option<&Config>, reason: &str, changed: Vec<Change>, started: f64) -> Decision {
    Decision {
        version: 1,
        mode: config.map_or(Mode::Shadow, |c| c.mode),
        model: config.map_or("unknown".to_string(), |c| c.model.id.clone()),
        full: true,
        reason: reason.to_string(),
        ecosystems: None,
        affected: None,
        base: None,
        head: None,
        changed,
        tests: IndexMap::new(),
        model_usage: None,
        job: None,
        inputs: None,
        ms: elapsed(started),
    }
}
