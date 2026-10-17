# picky — plan v3

A drop-in CI time saver that picks which jobs and tests run for a change.
- A static, symbol-level dependency graph decides what must run.
- History of past PRs and CI runs adds the links the graph can't see.
- A model (Jev by default) judges what is still uncertain. Each request sees the hunks its candidates need, at the right level of detail, plus a tolerance margin. Safety nets catch what the relevance signals miss.
- On any doubt, error or timeout, everything runs.

The core is a single-file CLI that works in any CI. The GitHub Action is a thin wrapper around it.

## Build status (2026-10-17)

The build is v0.2. It's tested locally (93 unit tests, plus end-to-end runs with real PHPUnit 11, ESLint 9, Prettier 3 and tsc) and **on GitHub-hosted runners** through the private fixture repo `ahmedragab20/picky-e2e`:
- 6 jobs set up by `picky init`: the decide job, bash JS and PHP jobs, Windows (pwsh), `node:22-alpine` and node-less `composer:2`
- 4 PRs: PHP change, TS change, docs only, a deliberately broken PHP class that must fail

### Done

| Area | What works | Plan milestone |
|---|---|---|
| Core | merge-base diff (PR base, `PICKY_BASE`, default branch); fail-open; decision reuse per commit; `picky explain`; `picky -- <cmd>` | M1 (part) |
| Gates | full run on `.picky.yml`, workflows, `critical` paths and non-PR events. **Per-ecosystem gates:** a JS lockfile or config runs only the JS commands in full; `composer.*` or `phpunit.xml` runs only the PHP commands in full | M1 (part) |
| JS/TS graph | file-level imports: relative, `.js`→`.ts`, index files, tsconfig `paths`, workspace packages, dynamic imports, deleted files | M3 (file level only) |
| PHP graph (first party) | class-level: every declared class or function is indexed (PSR-4, classmap or anything else); `use` (aliases, groups, functions), same-namespace names, docblock and string class names, `require`/`include`, `Pest.php` and `phpunit.xml` bootstrap edges, users of a deleted class | M6 (moved up) |
| Test runners | vitest, jest, `node --test`, PHPUnit, Pest, `php [-d k=v] vendor/bin/…`; npm/pnpm/yarn and composer scripts. PHPUnit 9 is narrowed only to one path, because it silently ignores the rest | M1 |
| Lint narrowing | ESLint (flat config): changed files plus their dependents, with `--no-warn-ignored`. Prettier: changed files, with `--ignore-unknown`. A change to the linter's config, or to a file the config imports, runs it in full. Legacy `.eslintrc` and `import/no-unused-modules` fall back to skip-or-full | new |
| Type checks and analyzers | tsc, vue-tsc, svelte-check, biome, phpstan, psalm, phpcs, php-cs-fixer, pint, rector: skipped when no changed file can matter to them (docs, media, styles, another language), otherwise run in full | new |
| Job-level setup skip | `npm ci`/`install`, `pnpm`/`yarn`/`bun install`, `composer install` and `npx playwright install` are skipped when every other command in the job is a marked command that skips. The job is read from the workflow (`GITHUB_WORKFLOW_REF`, `GITHUB_JOB`). Any unmarked command, non-bash step, expression working directory or non-passive `uses:` step keeps the setup | M2 (part) |
| Hook | `BASH_ENV` hook, `# picky` markers inside scripts **and** as the YAML comment of a one-line `run:` (YAML drops that comment, so picky reads it from the workflow), shell flags preserved, original line on any failure | M2 (part) |
| One decision per PR | `picky decide` in a small `picky` job computes the decision once into `$RUNNER_TEMP/picky`; the job uploads it as an artifact, and every other job (`needs: picky`) downloads it there and reuses it. Every step of the `picky` job is `continue-on-error`, so it never blocks the jobs that need it. **Verified with live Jev on 2026-10-17:** 1 model call per PR (before: 1 per job, with answers that differed near the threshold); every job prints the same decision. Cost: the `picky` job takes 7–12 s before the other jobs start. Fail-open: `picky decide` always exits 0, and a job without the artifact decides on its own | M2 (part) |
| `picky init` and the vendored bundle | `npm run build` makes one minified file (3.8 MB, Node 20+). `picky init` shows a diff, and `--write` applies it: `.picky.yml` (shadow), `.github/picky/` (hook, wrapper, bundle), the 2 env lines, `# picky` on every recognized command, and the decide job when several jobs or a matrix use picky. Workflows are edited as text at parser positions, so formatting and comments are kept; it is idempotent. Pull-request workflows only; Windows, container and non-bash jobs get the `shell: picky {0}` fallback (next row). The hook finds the vendored bundle next to itself, so `PICKY_BIN` is no longer needed. **Verified on 2026-10-17:** the fixture's workflow was reset to plain commands and set up by `init` alone; all 4 PRs behaved as before. Found and fixed: `init` put two spaces before `# picky`, which the repo's own `prettier --check` rejected | M2 (part) |
| `shell: picky {0}` fallback | For Windows, container and non-bash steps. GitHub hands picky the step script; picky decides every command (setup commands by the job-level rule) and runs the result in bash, sh, pwsh or powershell with GitHub's flags and PowerShell wrapper. A vendored local action (`uses: ./.github/picky`) puts it on PATH (`picky.cmd` on Windows); in node-less containers the wrapper uses the runner's node under `/__e`, and without any node the step runs unchanged. `init` sets it up. **Verified on 2026-10-17** with a Windows job (pwsh), `node:22-alpine` and node-less `composer:2`: narrowing, setup skips and the failing PHP run all behave as in the bash jobs, and every job reuses the shared decision. **Found and fixed:** git refuses a container workspace ("dubious ownership"), so picky's read-only git calls pass `safe.directory`; in a container `runner.temp` is the host path, so init downloads to `/__w/_temp`; Windows checks out `.picky.yml` with CRLF and writes the event JSON differently, so the cache key normalizes both. A job that can't reuse a shared decision now logs which input differed | M2 (part) |
| Model | Jev `noul` judge for tests outside the floor, retries, 30 s budget, usage line (questions, requests, tokens) | M1 (part) |
| Modes and report | shadow and enforce; log and job summary | M8 (part) |
| M0 spike | **Passed on ubuntu-latest (Node 22, PHP 8.3):** workflow-level `BASH_ENV` with `${{ github.workspace }}` works; `$0` is the step script; one-line `# picky` YAML comments are honored; the step script text matches `run:`, so job-level skip works (`composer install` skipped on the TS and docs PRs); a failing narrowed PHPUnit run fails the job. Decisions take 0.6–1.1 s. **Found and fixed:** the event's base sha goes stale when the base branch moves, so picky diffs against the first parent of GitHub's merge commit (fetched at depth 1) | M0 |

### Pending, in release order

1. **M2 remainder.**
   - pinned, checksum-verified download in `hook.sh` (the bundle is vendored for now, which git already pins)
   - the `picky` workflow: graph and history artifacts, `workflow_run` learning, nightly. It's only useful once M3's graph cache and M4's history exist
   - upstream-job skipping through `needs:`
2. **Shadow recall report (M8).** It shows whether a test picky would have skipped failed, which tells users when it's safe to switch to `enforce`.
3. **M1 remainder:**
   - `extends` and layered config
   - `config print`/`validate` and the schema
   - risk inference and `[picky: …]` annotations
   - label or commit-trailer override
   - `php artisan test`, which isn't verified, so it currently runs in full
4. **M3 remainder:**
   - symbol-level analysis
   - need-based context packing (diffs over 200k characters currently run every candidate)
   - lossless compaction
5. **Later milestones:**
   - M4 history and backstop
   - M5 Go and Rust
   - M7 other model providers
   - M9 other CIs and binaries
   - M10 coverage graph
   - M11 ordering and sharding
   - M12 release

## Principles

1. **The action itself never fails CI.** Any error, timeout or bad response gives `mode=full`, so everything runs.
2. **Deterministic evidence first, the model last.** The graph and history make up the floor. The model only judges the remainder, and removes tests only in opt-in `prune` mode.
3. **Unseen means run.** The model never decides on a candidate against a diff it didn't fully see. If any content can't be shown (budget, context limit or timeout), the affected candidates run.
4. **Need-based context with a margin, never blind spots.**
   - Each request gets the hunks its candidates need, at the right level of detail, plus a tolerance margin.
   - Every change is always visible as a pointer.
   - Unclaimed hunks are swept, uncertain answers get a deeper pass, and sampled full-context audits widen the margin automatically.
   - Commit count never matters.
5. **Learns, but only in the safe direction.** History can add tests to the floor and tune the threshold toward higher recall. It never makes picky skip more than the graph allows.
6. **Shadow mode before enforcement**, with recall measured on the repo's own history.
7. **Fast, or out of the way.** Target p50 under 10s with a hard budget of 30s. Over budget means a full run.
8. **Zero restructuring.** Config lives in `.picky.yml`, overrides go in the workflow's top-level `env:`, and a step opts in with a `# picky` comment. Jobs, steps and `needs:` stay as they are.
9. **Selective on PRs, full before or after merge.** The merge queue or post-merge full run is the backstop, and every miss it finds becomes history.
10. **Critical work never depends on the model.** Anything marked `critical` (in config, inferred, or with a `[picky: critical]` annotation) is decided by deterministic rules only.
12. **Simple config.** Six optional top-level keys. Tuning is automatic, and safety settings can only be tightened, never loosened, by later layers.
11. **Humans can always override.** A label or commit trailer forces a full run.

## Where the time savings come from (ranked)

1. **Skipped job work.** When none of a job's marked steps are selected, picky also skips its install and setup commands. No `if:` lines are needed.
2. **Narrowed tests.** Jobs that do run get only the affected test files, packages or crates.
3. **picky stays cheap.**
   - It fetches two trees, not the history.
   - The graph is cached and updated incrementally.
   - One batched Jev call.
   - A pinned, checksum-verified binary is fetched once per job and kept in the tool cache.
4. **"Time saved" shown on every run**, from historical durations.

## Large PRs (thousands of files, hundreds of commits)

- **Commit count is irrelevant.**
  - picky gets the merge-base SHA from the API (GitHub compare, GitLab MR), or by deepening the fetch step by step in generic CI.
  - It then fetches only the merge-base commit (`--depth=1`) and diffs the two trees locally, with rename detection.
  - So 5 commits or 500 cost the same. Reverted work cancels out in the net diff.
- **No file-count bailout.** The old `max-changed-files → full` gate is gone. The time budget is the only bailout.
- **Parallel analysis.** Hunk parsing runs in a `worker_threads` pool.
- **Cosmetic changes are dropped from the test floor (deterministic).**
  - A hunk counts as cosmetic when its AST is unchanged after stripping whitespace and comments. Lint and format tasks still run for it.
  - Comments that change behavior always count as code:
    - Go `//go:` directives
    - Rust doc comments (they are doctests)
    - PHP docblock annotations
    - TS `@ts-` pragmas and bundler magic comments
- **Codemods collapse into one change, without losing content.**
  - Hunks with the same normalized shape (identifiers abstracted) become one template plus a binding table: each file, its lines, and the identifiers substituted.
  - The model sees the exact change once and every location it applies to.
- **Moves and renames:**
  - Moved blocks are shown as `moved A:10-40 → B:5-35`, with any edits made inside the move shown as a hunk.
  - Files renamed at 100% similarity are shown as a pointer only.
- **Generated files** (`.gitattributes linguist-generated`, `*.min.*`, codegen outputs) are shown as a pointer plus their source, which is diffed normally.
- **Commit subjects are cheap signal.** They are deduped and capped at 20 ("update snapshots", "add migration"), and sent as part of the state.

## Symbol-level change analysis (precise floor)

- Each hunk is mapped to the enclosing top-level symbol, using tree-sitter on the before and after trees.
- Each symbol gets a change kind: `added`, `removed`, `renamed`, `signature`, `body`, `cosmetic`, `side-effect`.
- The floor uses the dependents of changed exported symbols, not of whole files. It falls back to the file level in these cases:
  - top-level side effects
  - a changed non-exported symbol used by exports
  - `import *` or dynamic imports
  - an unresolved re-export
- Precision varies by ecosystem:

| Ecosystem | Test unit | Floor precision v1 |
|---|---|---|
| JS/TS | test file | symbol (follows barrel `export … from` chains) |
| Go | package | symbol (`pkg.Symbol` selector references) |
| PHP | test file | class (PSR-4: one class per file) |
| Rust | crate, integration target | crate (symbol-level later) |

- **Config and data files** (`.env`, YAML, JSON, SQL, PHP config arrays) get a key-level diff with the old and new values, such as `config/app.php session.lifetime: 120 → 30`. This is lossless and more compact than a line diff of a reformatted file.

## Model context: need-based, with a tolerance margin

**Goal:** each model request sees what its candidates need, in the detail they need, plus a deliberate margin. Accuracy is protected by four things:
1. a sweep for changes nothing claims
2. a deeper second pass on uncertain answers
3. sampled full-context audits that widen the margin automatically when they catch a miss
4. a one-line pointer for every change, present in every request

### 1. Relevance: which hunks a candidate needs

Every pair of a change unit (hunk or symbol) and a candidate test gets a relevance score from independent deterministic signals:

| Signal | Catches | Source |
|---|---|---|
| Graph distance | imports and calls, through re-exports | the dependency graph (hops from the changed symbol to the test) |
| Runtime coverage | reflection, dynamic dispatch, DI containers (e.g. Laravel), dynamic imports | per-test coverage from nightly full runs on main (file → tests executed) |
| Identifier and literal overlap | string-based DI, route, event, table and column names, env vars, config keys, fixture names | an inverted index of identifiers and string literals → files, built with the graph |
| History | links nothing in the code shows | co-failure and co-change between the hunk's path or symbol and the test |
| Type affinity | migrations ↔ DB tests, `.env`/config ↔ integration tests, snapshots ↔ their test | built-in rules, extendable in config |

The score is the max of the normalized signals. A single strong signal is enough, and signals are never averaged away.

### 2. Fidelity: how much of each hunk to show

Each hunk has four renderings. Cheaper renderings are always derived from the full one.

| Level | Rendering | Shown when |
|---|---|---|
| L3 | the complete new symbol plus its diff | high relevance (graph distance ≤ 1, or a direct identifier hit) |
| L2 | zero-context hunk with the symbol signature header | medium relevance |
| L1 | signature before/after, touched identifiers, +/- counts | inside the tolerance band |
| L0 | `path:lines#symbol kind` pointer | everything, via the overview |

Fidelity is not wasted on lines that don't help:
- Import-only edits render as `imports +X -Y`.
- Pure deletions render at L1 (the signature plus the symbol's callers), unless the candidate calls the deleted symbol, which promotes it to L3.
- Test-file hunks are shown only to candidates that share their helpers or fixtures.

### 3. Tolerance: the deliberate margin

- **`tolerance`** (default 2) widens the needed set by extra rings:
  - +1 graph hop per ring
  - one fidelity level up for borderline hunks
  - all hunks in the same symbol cluster as a needed hunk (callers of a changed signature travel together)
- **Candidate grouping adds margin for free.**
  - Candidates with similar needed sets (MinHash similarity) share one request.
  - The request's state is the union of their needs, at the highest fidelity any of them requires.
  - Each candidate sees slightly more than it strictly needs, while state tokens are paid once for the group.
- **Packing:** groups are packed into requests of at most `pack-ratio` of the context window. The window is read from `/v1/models` at runtime. A group too big for one request is split by symbol cluster, and its candidates are asked in each part.

### 4. Safety nets

- **Orphan sweep:**
  - An orphan is a non-cosmetic hunk with zero relevance to every candidate (no graph link, no identifier hit, no history, no rule).
  - Orphans are shown at L2 to all remaining candidates in a sweep pass.
  - Nothing behavior-relevant can stay invisible just because the signals missed it.
- **Grey-zone escalation:**
  - Candidates whose p lands in `escalate` (default 0.1–0.6) are re-asked with context two levels richer:
    - tolerance +2 rings
    - L1 hunks promoted to L3
    - the related test helpers and fixtures included
  - The first pass is cheap and targeted; the second pass is deep, and only where the model was unsure.
- **Audit sampling:**
  - A fraction of runs (`audit-rate`, default 5%) is also judged in `full` mode: every hunk at L2 or above, asked about every candidate.
  - Any test kept in full mode but skipped in targeted mode is logged as a miss along with the hunk that caused it. picky then raises `tolerance` and adds the missing signal pattern to the repo's rules.
  - Audits run in shadow and never block or slow the selection result.
- **Unseen means run:** candidates not fully judged when the budget, timeout or `max-input-tokens` runs out go to the floor.

### 5. Lossless compaction (always on, before everything above)

- Cosmetic AST-equal hunks become pointers. Directive comments still count as code.
- Codemods become a template plus a binding table.
- Moves become `moved A → B` plus any edits inside the moved block.
- Generated files are shown as a pointer plus their source.
- Config and data files get a key-level diff with old and new values.

### Example request (one candidate group)

```
OVERVIEW  pr: "Rotate session tokens" | commits: add rotation; fix migration
  C1 src/auth/token.ts:40-71#verifyToken body | C2 src/auth/token.ts#TokenError added
  C3 config/session.yml lifetime 120→30 | C4 x214 import-rename T1 | C5 x38 cosmetic src/ui/**
DETAIL
  [L3] C1 src/auth/token.ts#verifyToken  <full new function + diff>
  [L2] C3 config/session.yml  lifetime: 120 → 30, +rotate_after: 10
  [L1] C2 TokenError(message, code) added; thrown by verifyToken
QUESTIONS
  q17 tests/auth/TokenTest.php covers[testRejectsExpired, testRotatesAfter] uses[Auth\Token, fixtures/tokens.json] hist: failed 3/11 on token.ts
  q18 tests/session/SessionTest.php covers[testLifetime] uses[config session.lifetime]
```

## Learning from PR history

**Signals, stored per repo:**

| Signal | Source | Use |
|---|---|---|
| co-failure(path → test) | full-run results from the `::picky-result::` log lines, plus GitHub/GitLab job conclusions | history floor and question hint |
| co-change(path ↔ test) | `git log` of merged PRs | question hint, plus history floor at high support |
| flakiness(test) | fails with no related change, or passes on rerun | excluded from attribution and noted in the summary |
| durations(test, job) | `::picky-result::` timings, job timings | the "time saved" report and sharding |

**How history is used, deterministic first:**
1. **History floor:** a test is added to the floor when `co-failure >= k` (default 2) and `rate >= r` (default 0.2). This uses no tokens and no model.
2. **Hints:** a one-line `hist:` fragment in that candidate's question.
3. **Threshold auto-tune:** using shadow and full-run outcomes, the judge threshold is lowered automatically when recall drops below `target-recall` (default 0.995). It is raised only after N clean runs, and never above the built-in maximum (0.2).

**Attribution:**
- A failure in a run with F changed files is credited at 1/|F| to each changed path (and to its changed symbols when known). Huge PRs therefore don't pollute the stats.
- Observations decay with a 90-day half-life, so history follows the repo as it changes.

**Storage:** a compact binary map, under 5 MB for large repos.
- Default: the CI cache (actions/cache with rolling restore keys, or a GitLab cache).
- Durable option: `history: branch` writes to an orphan `picky-history` branch. This needs `contents: write`, so it's opt-in.

**Bootstrap:** `picky learn` runs in a scheduled weekly job, never on the PR path. It does a full fetch, builds the co-change map from the last N months of merges, and pulls the last N CI runs' failed jobs through the API.

## Speed design (picky's own runtime)

- **Diff:** merge-base from the API plus a one-commit fetch. If a patch is truncated, picky fetches that blob pair.
- **Graph cache:** keyed by manifest hashes plus `git ls-files -s` blob SHAs. Only changed blobs are re-parsed.
- **History load:** one cache restore of a file under 5 MB.
- **Lazy grammars:** only the WASM grammars for detected ecosystems are loaded.
- **Short-circuits:** no model call when nothing is left to judge, or when a gate forces a full run.
- **Decision reuse:**
  - A decision is keyed by (merge-base tree, head tree, config hash, model version, history version).
  - Re-runs and empty rebases reuse the stored decision with no model call. The result is the same by construction.
- **Parallel batches:** Jev batches run concurrently against a shared deadline.
- **Distribution:** one bundled JS file (node24), plus single-file binaries.

## Caching and costs

picky has no server. Caches live in the user's CI, compute runs on the user's runners, and tokens are billed to the user's API key.

| Cache | Contents | Where | Written by |
|---|---|---|---|
| Graph | dependency and symbol graph, identifier/literal index, coverage edges, keyed by blob SHAs | GitHub: artifact of the `picky` workflow. Elsewhere: CI cache. | the `picky` workflow on main pushes, at most once per `graph-save-every` commits |
| History | co-failure, co-change, flakiness, durations | GitHub: artifact of the `picky` workflow, or the opt-in `picky-history` branch. Elsewhere: CI cache. | the `picky` workflow, from trusted runs only (main, merge queue) |
| Decisions | `picky-decision.json` keyed by tree pair, config and model | the runner's temp directory (per job), plus the graph artifact for reruns | any run |
| Prompt cache | the shared overview and hunks across question batches | the model provider (Claude/OpenAI) | automatic |

**Why artifacts on GitHub:** the hook runs inside `run:` steps, which don't get the runner token that `actions/cache` needs. So:
- The `picky` workflow, which uses the JS action and does have that token, publishes the graph and history as artifacts.
- PR jobs download the latest ones through the REST API with `PICKY_GITHUB_TOKEN` (`actions: read`). This works on fork PRs too.
- PRs never publish.

**Lifecycle:**
- **Restore:** the latest artifact from main.
- **Incremental update:** `git ls-files -s` blob SHAs are compared with the artifact, and only changed files are re-parsed.
- **Publish:** from main only.
- **Pruning:** the nightly job deletes all but the newest 3 artifacts, so storage stays about 3 × (graph + history).

**Expected sizes (to be confirmed by the M8 benchmark):**

| Repo | Source files | Graph (zstd) | History |
|---|---|---|---|
| small | ~1k | <1 MB | <1 MB |
| mid | ~10k | ~5 MB | 1–3 MB |
| large monorepo | ~100k | ~50–70 MB | 5–10 MB |
| Chromium-scale | ~400k+ | ~250–400 MB | 10–20 MB (capped at `history.max-mb`) |

**Who pays:**
- **Storage:** the repo's CI plan.
  - On GitHub, artifacts count toward the account's Actions storage quota, so picky keeps only the newest 3.
  - Elsewhere, the CI cache quota.
  - Public repos pay nothing.
- **Runner minutes:** the repo's plan. This covers picky's own seconds inside marked steps, plus the `picky` workflow: main warm-up, learning from `workflow_run`, and the nightly coverage and learn jobs.
- **Model tokens:** the user's API key.

The summary's "time saved" shows whether the jobs picky skips outweigh picky's own cost.

## Pipeline

1. **Detect CI:** base, head, event, PR number, branch. Adapters cover GitHub, GitLab, CircleCI, Buildkite, Bitbucket and Jenkins, with generic flags as a fallback.
2. **Diff:** merge base, two trees, net diff with renames, plus commit subjects.
3. **Gate:** these run everything:
   - a `picky:full` PR label or a `[picky full]` commit trailer
   - a full-run event or branch, including the merge queue (`merge_group`)
   - lockfile or CI config changes
   - a missing API key
   - over the time budget
4. **Caches:** check for a reusable decision first, and if one exists, emit it and stop. Otherwise restore the graph (with coverage edges) and the history, and update the graph incrementally.
5. **Analyze:**
   - Map hunks to symbols and change kinds.
   - Separate out cosmetic hunks; they stay listed as pointers.
   - Turn codemods into templates, and detect moves and generated files.
   - Build the overview, score relevance for each (hunk, candidate) pair, assign detail levels, add the tolerance margin, find orphans, group candidates and pack the requests.
6. **Floor (deterministic):** these always run:
   - tasks whose `paths` match
   - changed tests
   - dependents of changed symbols (or files, classes, crates)
   - tests whose recorded coverage includes a changed file
   - the history floor
   - `critical` tasks and tests: run whenever any deterministic rule (paths, graph, coverage, history) matches, and never sent to the model
   - tests marked `always`, and tests linked by `affects`/`covers` annotations
   - `low` tasks: decided here by direct path match only, and never sent to the model
7. **Judge:**
   - Pass 1: parallel requests, each made of the overview, its group's needed hunks and that group's questions, plus the orphan sweep.
   - Pass 2: grey-zone candidates are re-asked with richer context.
   - Max-aggregate p across requests. A candidate is kept if `p >= threshold`.
   - Anything not fully judged runs.
   - Sampled audits run the full-context mode in shadow.
8. **Prune (opt-in):** transitive dependents may be dropped when `p <= prune-threshold`.
9. **Emit:**
   - `picky-decision.json` (with the context each candidate saw and its reasons), CI outputs, and a summary with time saved and the blast radius.
   - Selected tests are **ordered** by p (highest first, for fail-fast).
   - Selected tests are **sharded** into `shards` balanced parallel groups using historical durations.
10. **After the tests:** marked test commands print a `::picky-result::` line. The `picky` workflow (`workflow_run`, trusted events only) reads those lines and the job conclusions, updates the history and auto-tunes the threshold.
11. **Backstop (merge queue or post-merge full run):**
    - A failure in a test picky skipped on the PR is a **miss**.
    - picky bisects automatically to the PR that caused it, using the stored decisions plus a re-run.
    - It records the miss in the history (co-failure, plus the hunk → test pattern) and raises `tolerance` if the audit shows a context gap.
    - It reports the miss on the summary.
12. **Nightly coverage (`picky coverage`):** a full run on main with coverage enabled records file → tests edges into the graph cache. It uses the c8/V8 coverage of vitest/jest, `go test -coverprofile`, `cargo llvm-cov` and PCOV/Xdebug.

## Seamless setup (GitHub)

**Goal:** a team adopts picky without restructuring its CI. The AI and selection config lives in one global place. A workflow can override it at the top of the file. A step only says "this step uses picky".

### Where each thing lives

| What | Where | Who edits |
|---|---|---|
| Model, mode, risk globs (all config) | `.picky.yml` at the repo root, optionally `extends` an org preset | repo or platform owners |
| Per-workflow overrides | the workflow's top-level `env:` (`PICKY_MODEL`, `PICKY_MODE`) | workflow owner |
| "This step uses picky" | a `# picky` comment on the `run:` line | anyone |

### What a workflow looks like

```yaml
name: CI
on: [pull_request, push, merge_group]

env:                                                    # once, at the top
  BASH_ENV: ${{ github.workspace }}/.github/picky/hook.sh
  PICKY_API_KEY: ${{ secrets.PICKY_API_KEY }}
  PICKY_GITHUB_TOKEN: ${{ github.token }}
  # PICKY_MODEL: anthropic/claude-sonnet-5-5           # optional override of .picky.yml

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
      - run: npm ci
      - run: npx eslint .        # picky
      - run: npx vitest run      # picky
      - run: ./scripts/build.sh  # (not flagged: always runs, untouched)
```

Jobs, steps, `needs:` and matrices all stay as they are. The diff to an existing workflow is 3 `env` lines plus a `# picky` comment on each step picky should decide.

### How it works

1. **The hook:** `.github/picky/hook.sh` is a tiny bootstrap committed by `picky init`, about 30 lines.
   - Bash sources it before every `run:` step, through `BASH_ENV`.
   - It checks the step's script (`$0`) for `# picky` markers.
   - With no marker, it returns immediately and the step runs exactly as before.
2. **On the first marked step in a job**, the hook:
   - fetches the pinned picky binary and verifies its SHA-256 against the value in the hook, then caches it in the runner's tool cache
   - restores the graph and history
   - computes the decision for this job
3. **Marked lines:**

| The marked line is | picky does |
|---|---|
| a known command (`vitest`, `jest`, `playwright`, `eslint`, `biome`, `prettier`, `tsc`, `phpunit`, `pest`, `phpstan`, `phpcs`, `go test`, `go vet`, `golangci-lint`, `cargo test`, `cargo clippy`, `npm/pnpm/yarn/composer run <script>` resolved through the manifest, or a repo shell script picky can read through) | skips it, narrows it to the affected tests or changed files, or runs it in full |
| anything else | treats it as a whole step: deterministic rules first, then the model judges it from the step name, the command and the script contents |

   Unmarked lines in the same step run untouched.
4. **Job-level skip without `if:`:**
   - At its first run, picky reads the workflow file (`GITHUB_WORKFLOW_REF`, `GITHUB_JOB`) and finds every marked step in the job.
   - If all of them are skipped, it also skips the job's recognized setup commands (`npm ci`, `composer install`, `go mod download`, …), turning them into exit 0 with a note.
   - Unrecognized unmarked steps still run.
   - Upstream jobs whose downstream marked steps are selected always run, following the workflow's `needs:` graph.
5. **Results:**
   - picky runs test commands with an extra machine-readable reporter, so there's no report step to add.
   - It prints one `::picky-result::` line in the step log.
   - The `picky` workflow (below) reads those lines through the API to learn.

**Step identity**, used for history and the summary, is `<workflow>/<job>/<step name, or a hash of the command>`. It survives steps being reordered.

### The `picky` workflow (generated by `picky init`, one file)

`.github/workflows/picky.yml` uses the picky action, which has the runner token, for everything a `run:` step can't do:

| Trigger | Job |
|---|---|
| push to main | warm the graph and publish it as an artifact (at most once per 20 commits) |
| `workflow_run` of CI workflows (main and `merge_group` only) | read the `::picky-result::` lines and job conclusions through the API, update history, run the backstop and auto-bisect |
| nightly schedule | coverage run, `picky learn`, prune old artifacts |

PR jobs download the latest graph and history artifacts through the REST API with `PICKY_GITHUB_TOKEN` (`actions: read`). That works on fork PRs too, which get read-only access.

### Setup: 3 steps

1. `npx picky init`:
   - writes `.picky.yml`, `.github/picky/hook.sh` and `.github/workflows/picky.yml`
   - adds the 3 `env` lines to your workflows
   - marks the steps it recognizes with `# picky` and shows the diff for you to review
   - Every marker is optional; delete any you don't want.
2. Add the `PICKY_API_KEY` secret.
3. Merge. picky starts in `shadow` mode. Once the summary says it's ready, set `mode: enforce` in `.picky.yml`.

### Fallbacks

- **Steps the hook can't reach** (Windows, containers, `sh` or `pwsh` steps): `picky init` adds `uses: ./.github/picky` after checkout and runs those steps with `shell: picky [shell] {0}`. picky decides every command of the step, then runs it in the real shell. (`picky/setup@v1` replaces the local action once picky is published.)
- **If picky can't run at all** (no node, missing file): every step runs unchanged. That counts as a full run.

### Other CIs

These have no `BASH_ENV`-style hook into the step script, so commands get a prefix: `picky -- npx vitest run`.
- **GitLab:** picky is installed in `default.before_script` and configured through top-level `variables:` (`PICKY_MODEL`, `PICKY_MODE`).
- **Everything else:** the binary plus the prefix.

## Model layer (one interface for every provider)

### Configure: one secret, one line

**Secret:** `PICKY_API_KEY` for every provider. If it isn't set, picky falls back to the provider's own variable: `TYPESAFE_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` or `OPENROUTER_API_KEY`. An explicit `key-env` (object form) takes precedence over both.

**Model:** a `provider/model` string:

```yaml
model: jev                                   # shorthand for typesafe/jev-<pinned version>
model: anthropic/claude-sonnet-5-5
model: openai/<model-name>
model: openrouter/typesafe/jev-latest        # any model through OpenRouter
```

**Object form, only when needed:**

```yaml
model:
  id: openai-compatible/my-model
  base-url: https://llm.internal/v1          # LiteLLM, Vercel AI Gateway, vLLM, ...
  key-env: MY_LLM_KEY                        # env var NAME; the key never appears in config
  context: 128000                            # only for models missing from the registry
  fallback: anthropic/claude-haiku-5-5       # tried on outage before a full run
```

- **Per-workflow override:** `PICKY_MODEL` in the workflow's top-level `env:` overrides `.picky.yml` for that workflow.
- **Switching providers:** edit the `model` line in `.picky.yml` and change the secret's value. No workflow file changes.

### Behind the line

```ts
interface Judge {
  decide(state: string, questions: Record<string, Question>, deadline: number): Promise<Record<string, number>>
}
```

1. **Adapters:**
   - **`typesafe`:** native `noul` questions on `/v1/systemone`.
   - **`anthropic`, `openai`, `openrouter`, `openai-compatible`:** the same requests (overview, needed hunks, questions) through structured JSON output, validated strictly.
   - Everything above the adapter (selection, safety nets, history) is identical across providers.
2. **Model registry:**
   - It lists each known model's context window, whether its probabilities are calibrated, prompt caching support and pricing.
   - Packing, prompt caching and the cost summary adapt to it.
   - For unknown models, picky reads `/v1/models`, falling back to the object form's `context`.
3. **History learned per model:**
   - Thresholds are auto-tuned per model id.
   - A model with no history starts at a conservative (low) threshold until it has enough runs.
4. **Safe switching:**
   - Changing `model` is a config change, so that PR gets a full run.
   - `picky eval --model <id>` replays past PRs on a candidate model. It reports recall, selection size and cost next to the current model.
5. **Fallback chain:** on an outage (429, 529 or 5xx past retries), picky tries the primary model, then the fallback model, then a full run.
6. **Retries:** each adapter retries 429/529 with backoff, within the shared deadline.

### Privacy

**`send: filenames`** (under `advanced:`) sends only the overview, with paths, symbols and change kinds, and no code. The README states plainly that it lowers accuracy.

## Config (kept simple)

### The whole config: 5 top-level keys, all optional

```yaml
# .picky.yml
extends: github:my-org/.github/picky.yml@v1   # optional org preset
mode: enforce                                 # shadow (default) | enforce
model: jev                                    # provider/model, e.g. anthropic/claude-sonnet-5-5 (see Model layer)
critical: ["**/migrations/**", "tests/security/**", "deploy/*"]   # path globs, or <job> / <job>/<step> ids
low: ["docs/**", "**/*.md"]
```

**Not in the config on purpose:**
- **Tuned automatically:** `threshold` (from history), `tolerance` (raised by audit misses), shards, ordering.
- **Fixed safe defaults:** escalation, audits and packing. They go in an `advanced:` block that the getting-started docs never show.
- **No task definitions.** Tasks are the `# picky`-marked steps of your existing workflows.
- **Always on:** full-run triggers (lockfiles, CI config, main, merge queue). Repos can add paths under `advanced.full-run`, never remove them.

### Risk levels

| Risk | Decided by | Model? | Default for |
|---|---|---|---|
| `critical` | Any deterministic link: paths, graph (even indirect), coverage, history floor. With no rules, it always runs. | never; `prune` never applies | jobs with a deployment `environment:`, `deploy*`/`release*`/`migrat*` jobs |
| `normal` | floor plus the model | yes | everything else |
| `low` | direct path match only | never | tasks that only touch docs or markdown |

**Inferred risks:**
- Each one is shown in the summary with its reason, e.g. `deploy-web → critical (environment: production)`.
- Inference can raise a task to `critical` but never lower one to `low`.

### Code annotations

**Syntax:** `[picky: <directive> <args>]` inside any comment (`//`, `#`, `/* */`). An unknown or misspelled directive shows a warning in the summary.

**What it attaches to:**
- **File header:** the whole file.
- **Directly above** a function, class or test: that symbol.

| Directive | In a source file | In a test file |
|---|---|---|
| `critical` / `normal` / `low` | changes to this symbol use that risk; every test linked to it follows | this test's risk |
| `always` | — | runs on every PR |
| `affects <glob>` | changes here also select tests matching the glob | — |
| `covers <glob>` | — | changes matching the glob select this test |

- Annotations are collected while the graph is built and cached with it.
- Adding, removing or changing an annotation is never treated as cosmetic. The tests in its scope run.

### Layers and one merge rule

Layers apply in order: built-in → org preset (`extends`) → `.picky.yml` → code annotations → the workflow's top-level `env:` (`PICKY_MODE`, `PICKY_MODEL`, `PICKY_API_KEY`) → PR overrides (label or trailer, which force a full run only).

**The rule: later layers win, except that safety only goes up.**
- Risk takes the highest level across all layers.
- `critical`, `always`, `affects`, `covers` and the full-run paths accumulate.

So an org preset that marks `**/migrations/**` as critical can't be undone by a repo. No separate policy block is needed.

Monorepos don't need per-directory config files: globs in `critical`/`low` and code annotations cover per-package needs.

### Safety

1. **A PR can't loosen the rules for itself.**
   - A change to `.picky.yml` or the `extends` ref triggers a full run.
   - Annotations from the PR's head can only raise risk or add links. Lowering a risk, or removing `always`/`affects`/`covers`, takes effect only after merge, once reviewed.
2. **`extends` must be pinned** to a tag or SHA; a branch ref gets a warning.
   - Presets are cached by ref.
   - If a preset can't be fetched, picky does a full run.
   - Private presets use a token, so fork PRs get a full run.
3. **YAML or JSON only.** Config never executes code.

### Tooling

- **`picky init`:** first-time setup.
- **`picky config print`:** shows the effective config and the layer each value came from.
- **`picky config validate`:** checks the config against the schema.
- **A published JSON Schema:** gives editor autocomplete.

## Security

- The API key is read only from inputs or the environment and masked; picky never logs it.
- Not designed for `pull_request_target`. Fork PRs get a full run.
- Diff text and commit messages are untrusted.
  - Cards carry pointers and change kinds, not free text, except PR and commit subjects, which are length-capped.
  - The model can only shift probabilities, and the floor still holds.
- History is read-only on fork PRs, so untrusted runs can't poison it. History writes happen only on trusted events (push to the default branch, or same-repo PRs).
- The README documents data egress, and how to reduce it with `send: filenames`.

## Proving it works

- **Unit tests:**
  - one fixture repo per resolver, with golden tests for diff → overview → floor
  - cosmetic detection, including every directive-comment exception
  - codemod clustering
- **Context tests (property-based):** for random diffs and candidate sets:
  - every change appears in every overview
  - every non-cosmetic hunk is shown at L2 or above to every candidate with relevance ≥ the tolerance band, or to all candidates via the orphan sweep
  - no request exceeds the context window
  - template plus bindings rebuild the original hunks exactly
- **Relevance tests:** each signal (graph, identifier/literal, history, type affinity) has fixtures. For example, a string-based DI or config-key link must reach its test without any import edge.
- **Escalation and audit tests:**
  - a grey-zone p triggers pass 2 with strictly richer context
  - an audit miss raises `tolerance` and records the pattern
  - audits never change the selection
- **Targeted vs full agreement:** `picky eval` runs both modes on replayed PRs and reports how often their decisions disagree. The goal is zero missed failures in targeted mode.
- **History tests:**
  - attribution math
  - decay
  - flaky exclusion
  - auto-tune only moves toward recall
  - fork runs can't write
- **Judge tests:** recorded responses only. One opt-in live smoke test.
- **Fail-open tests:** a 5xx, a timeout, malformed JSON, a missing key, a budget overrun, a missing decision file, and a corrupt history cache must each give a full run.
- **Large-PR benchmark:** a synthetic 5k-file, 500-commit PR on a large public monorepo. Targets: warm p50 under 10s, 100% hunk coverage, input tokens reported per run, and the cache sizes and restore times measured to confirm the table in Caching and costs.
- **Feature tests:**
  - **Overrides:** the label and the trailer each force a full run.
  - **Risk:** `critical` and `low` work never appears in a model request. Inference never lowers a risk.
  - **Annotations:**
    - parsing in all four ecosystems, for both file-header and symbol scope
    - unknown directives warn
    - head annotations can only raise risk; lowering applies after merge
    - annotation edits are never cosmetic
  - **Model layer:**
    - every id form parses
    - the key is resolved from `key-env` (when set), then `PICKY_API_KEY`, then the provider env var
    - a key in a config file is rejected
    - the fallback chain goes primary → fallback → full on a 5xx
    - a new model starts at the conservative threshold
    - a `model` change triggers a full run
    - every adapter passes the same recorded-response contract suite
  - **Hook:**
    - an unmarked step runs byte-for-byte as plain bash
    - only marked lines are touched, and `-eo pipefail` semantics are kept
    - a checksum mismatch, a missing binary or no network makes every step run unchanged
    - job-level skip never skips an unrecognized unmarked step
    - an upstream job always runs when its downstream marked steps are selected
    - step identity survives reordering
  - **Init:** on fixture workflows (single job, matrix, monorepo, reusable workflows), `picky init` produces a minimal diff, and the workflow behaves the same when picky is absent.
  - **Config layers:**
    - later layers win, except that safety only goes up
    - a repo can't undo an org preset's `critical`
    - a config change triggers a full run
    - an unreachable preset triggers a full run
    - `config print` shows where each value came from
  - **Decision reuse:** identical trees produce the same decision with zero model calls.
  - **Cache saves:** PRs never save the graph cache.
  - **Backstop:** auto-bisect blames the right PR on a fixture history.
  - **Coverage:** ingest adds file → test edges for each ecosystem.
  - **Sharding:** shards stay within 10% of each other's duration on fixture timings.
- **Replay eval:** `picky eval` replays the last N merged PRs of a public repo against its known CI failures and reports recall, selection size and tokens. This is the headline accuracy number for the README.

## Milestones

| # | Scope |
|---|---|
| M0 | Spike on GitHub-hosted runners. Confirm that:<br>• workflow-level `BASH_ENV` is sourced by `run:` steps<br>• `$0` is the step script inside the hook<br>• a missing hook file before checkout doesn't fail the step<br>• `github.workspace` works in workflow-level `env:`<br>• artifacts and job logs can be read with `github.token`, including on fork PRs |
| M1 | Core CLI: CI adapters (GitHub, generic), merge-base two-tree diff, gate with overrides, layered config (`extends`, merge rule, workflow `env` overrides, `config print/validate`, schema), path floor, risk levels and inference, command recognizers, `picky -- <cmd>` exec, Jev judge, budget, fail-open, decision reuse |
| M2 | GitHub integration:<br>• `hook.sh` with a pinned checksum, `# picky` markers, workflow and `needs:` reading, job-level skip of setup commands<br>• the `picky` workflow (warm, `workflow_run` learning, nightly) and artifacts over REST<br>• `picky init` (workflow edits shown as a diff), the `picky/setup` + `shell: picky {0}` fallback, summary |
| M3 | JS/TS graph and symbol analysis, `[picky: …]` annotations, cosmetic detection, codemod templates, move detection, identifier/literal index, relevance scoring, detail levels L0–L3, tolerance, candidate grouping and packing, orphan sweep, escalation, incremental cache |
| M4 | History: `::picky-result::` capture and ingest, store, attribution, history floor, hints, auto-tune, `picky learn`, merge-queue backstop with auto-bisect |
| M5 | Go (symbol) and Rust (crate) |
| M6 | PHP (class) |
| M7 | Unified model layer: `provider/model` ids, object form, `PICKY_API_KEY` with fallbacks to provider env vars, model registry, per-model thresholds, fallback chain, `picky eval --model`, adapters for Anthropic, OpenAI, OpenRouter and OpenAI-compatible |
| M8 | Shadow mode, audit sampling, `picky eval` replay (targeted vs full), large-PR benchmark |
| M9 | GitLab, CircleCI, Buildkite, Bitbucket, Jenkins adapters, binaries |
| M10 | Coverage-derived graph: `picky coverage` ingest for all four ecosystems |
| M11 | Test ordering by p. Duration-balanced sharding: when a marked command already shards (`--shard 2/4` in vitest, jest or playwright), picky replaces its split with one balanced by duration, with no config needed. |
| M12 | Marketplace and npm release, README with eval numbers, cost and cache docs |

Stack: TypeScript, node24, rollup single-file bundle, vitest, web-tree-sitter, `worker_threads`.

## Open items

- ~~**M0 spike results.**~~ Workflow-level `BASH_ENV` works on ubuntu-latest, so the hook stays the default (see Build status). Windows and container jobs work through the `shell: picky {0}` fallback (verified 2026-10-17).

- Check that the name `picky` is free on the Marketplace and npm.
- **Blocking for M3:** Jev's context window and its question limit per request, both read from `/v1/models` or found by probing. They set the request packing.
- Validate on the eval repos that max aggregation plus auto-tune keeps precision acceptable (selection size versus a full run).
- ~~Confirm that the PHPUnit/Pest multi-path arguments work.~~ Confirmed: PHPUnit 10.5/11.5 and Pest 3 run exactly the given paths. PHPUnit 9.6 runs only the first, so picky narrows it to a single path at most. Composer forwards paths only after `--`.
- **Found while building:** `run: cmd  # picky` on one line is a YAML comment, so it never reaches the step script. picky reads it from the workflow file. Confirmed on GitHub: the script text matches `run:`.
- Pick the public repos for `picky eval`: one per ecosystem, with a public CI history.
