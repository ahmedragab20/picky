---
name: picky
description: "Install and use picky, a CLI that runs only the tests, lint and type checks a pull request can affect in GitHub Actions (JS/TS, PHP, Kotlin/Java on Gradle, and Rust). Use it to install picky in a repo, migrate existing CI workflows to picky (`picky init`, `# picky` markers, `shell: picky {0}`), debug a picky decision or a `picky shadow miss`, or tune `.picky.yml`."
---

# picky

picky decides which tests, lint and type checks a PR can affect, then skips or narrows the CI commands you mark with `# picky`. A dependency graph decides first. A model (Jev) judges the tests the graph can't link. Any doubt, error or timeout runs everything.

- Works in GitHub Actions on `pull_request` events. Every other event runs everything.
- Ecosystems: JS/TS, PHP, Kotlin/Java (Gradle) and Rust.
- Docs: https://ahmedragab20.github.io/picky/ (full text: https://ahmedragab20.github.io/picky/llms-full.txt)

## Rules

1. Start in `mode: shadow`. Switch to `enforce` only after PRs show no real `picky shadow miss`, and only when the user agrees.
2. Never weaken, skip or delete a test to get a green run.
3. Mark only commands picky recognizes (see Supported tools). An unrecognized command runs in full anyway.
4. Show the `init` diff to the user. Apply it only after they agree.
5. Never print, log or commit the API key. Never commit, push or open a PR unless the user asks.
6. Leave workflow path filters and `--changedSince`-style flags alone unless the user agrees to change them.

## 1. Install

Needs Node 20+ on your machine and on the runners. Run from inside the repo:

```bash
curl -fsSLO https://github.com/ahmedragab20/picky/releases/download/v0.1.0-rc/picky.mjs
curl -fsSLO https://github.com/ahmedragab20/picky/releases/download/v0.1.0-rc/picky.mjs.sha256
sha256sum -c picky.mjs.sha256        # macOS: shasum -a 256 -c picky.mjs.sha256
node picky.mjs init                  # dry run: prints a diff, writes nothing
```

Stop if the checksum fails. Read the diff and the `picky:` notes at the end of the output (see "When init can't finish a job"). Show both to the user. After they agree:

```bash
node picky.mjs init --write
rm picky.mjs picky.mjs.sha256        # do not commit these two files
```

`gh release download v0.1.0-rc -R ahmedragab20/picky -p 'picky.mjs*'` also works.

`init` is safe to rerun: it never adds anything twice. It prints `already set up; nothing to change` when nothing is left. To upgrade, rerun it with a newer `picky.mjs`.

## 2. Migrate existing workflows

`init` writes these files:

| File | What it is |
|---|---|
| `.picky.yml` | config, in `mode: shadow` (only if the file doesn't exist) |
| `.github/picky/` | the hook, a `picky` wrapper, `picky.cmd`, a local action and the `picky.mjs` bundle. Git pins the version. Nothing is downloaded at run time. |

In each workflow that runs on `pull_request` it edits:

| Edit | Where |
|---|---|
| 2 top-level `env` lines: `BASH_ENV: ${{ github.workspace }}/.github/picky/hook.sh` and `PICKY_API_KEY: ${{ secrets.PICKY_API_KEY }}` | top of the workflow |
| `# picky` on every recognized command | a one-line `run:` (as a YAML comment) and each line of a block `run:` |
| A `picky` decide job, `needs: picky` and a download step in each picky job | when several jobs or a matrix use picky. One model call serves the whole PR. |
| `uses: ./.github/picky` after `actions/checkout`, and `shell: picky {0}` (or `picky pwsh {0}`, `picky sh {0}`) | Windows jobs, container jobs and `sh`/`pwsh`/`powershell` steps, where the bash hook can't reach |

It leaves alone: workflows that don't run on `pull_request`, jobs that call a reusable workflow (`uses:`), steps whose `working-directory` is an expression, `cmd` and custom shells.

A bash step is marked only in two forms: a one-line plain (unquoted) `run:` with no comment, and a block-scalar `run:` (`|`). Folded `>`, quoted and multi-line plain scripts are not marked.

Install commands (`npm ci`, `pnpm install`, `composer install`, `npx playwright install`, ...) are skipped when every other command in the job is a marked command that skips.

### Repos that already narrow CI

The PR that adds picky changes `.github/workflows/**`, so it always runs everything.

- **Workflow `paths:` filters** decide whether a workflow runs at all. picky doesn't replace them. Keep them unless the user asks to remove them.
- **`--changedSince`, `--changed`, `--onlyChanged`, `nx affected`, `turbo --filter`, `git diff`-based scripts**: ask the user whether to keep each one or replace it with picky. picky does not recognize a test-runner command with a bare word after it (a path or test filter), so such a command runs unchanged.
- Put the picky marker on the plain full-suite command (`npx vitest run`, `npx eslint .`, `vendor/bin/phpunit`). Never on a command that already targets specific files (`cargo test -p x`, `gradle :app:test --tests X`).
- To switch a command to picky, remove its narrowing flag in the same diff and tell the user. Do not do this without their agreement.

### Generated code

A step that generates code from files picky thinks are irrelevant (for example TS types built from PHP classes) can make a skipped type check wrong. Put those source files under `critical` in `.picky.yml`:

```yaml
critical: ["app/Models/**", "openapi/**"]
```

### When init can't finish a job

`init` prints a `picky:` note for each case. Fix it by hand only with the user's agreement:

| Note | Fix |
|---|---|
| `add BASH_ENV and PICKY_API_KEY to its top-level env by hand` | add the 2 env lines under the workflow's top-level `env:` |
| `job "x": add picky to its needs by hand` | add `picky` to that job's `needs` |
| `job "x" skipped: it needs actions/checkout before picky's setup step` | add `actions/checkout` as an early step, then rerun `init` |
| `not changed: no step runs a command picky recognizes` | nothing to do |
| `not changed: it does not run on pull_request` | nothing to do |
| `no workflows in .github/workflows` | add the env lines and `# picky` by hand |

A line joined with `&&`, a pipe, a redirect, `$VAR` or a glob, or split across lines with `\`, is not marked: picky runs it unchanged. Splitting it is a CI change: ask first. A one-line `run:` that already has a comment is not marked either.

## 3. What only the user can do

Tell the user these, exactly:

1. **Add the `PICKY_API_KEY` secret.** In the repo: **Settings → Secrets and variables → Actions → New repository secret**. Name: `PICKY_API_KEY`. Value: their Typesafe API key. Do not ask them to paste the key into the chat.
2. **Or choose graph-only mode.** Set `model: none` in `.picky.yml`. No model call is made and no diff leaves the runner. Tests the graph can't link are skipped, so keep shadow mode on longer.
3. **Commit and open a PR** (if you were not asked to). picky logs its decision on each PR.
4. **Switch to `enforce`** when PRs show no real `picky shadow miss`.

Until the secret exists and `model: jev` is set, picky logs `no API key (set PICKY_API_KEY)` and runs everything. Fork PRs have no secrets, so they also run everything.

## 4. Roll out

1. Merge the picky PR in `mode: shadow`. It runs everything.
2. On the next PRs, open the run's job summary and annotations. Look for `picky shadow miss`.
3. Read each miss (see below). Fix the cause or add `critical` paths.
4. When PRs go by with no real misses, set `mode: enforce` in `.picky.yml` (with the user's agreement).

A flaky test or a runner problem can look like a miss. Read the failure before acting.

## 5. Debug a decision

Run in a repo where picky is vendored:

```bash
.github/picky/picky explain                      # the decision for this branch (or: node .github/picky/picky.mjs explain)
PICKY_BASE=origin/main .github/picky/picky explain
PICKY_MODEL=none .github/picky/picky explain     # graph only, no API key needed
.github/picky/picky -- npx vitest run            # run a command narrowed to the change
```

- `explain` diffs committed work. Commit first.
- Without `PICKY_BASE`, the base is the merge base with the first of `origin/HEAD`, `origin/main`, `main`, `origin/master`, `master` that works. Else it prints `no base commit to compare against (set PICKY_BASE)`.
- Without an API key and with `model: jev`, it prints a full run. Set `PICKY_API_KEY` or `PICKY_MODEL=none`.
- Outside Actions, `picky -- <cmd>` follows `.picky.yml`'s mode: in shadow mode it runs the full command and logs what it would have done.

First line of the output: `picky: FULL RUN` or `selective`, with the mode, model and reason. Then one line per test file: `RUN` or `skip`, and why.

### Why did everything run?

| Reason | Meaning and fix |
|---|---|
| `.picky.yml changed`, `.github/workflows/... changed`, `.github/picky/... changed` | gate. Expected on the PR that adds or edits picky or CI. |
| `critical path <file> changed` | a `critical` glob matched. Narrow the glob if it's too broad. |
| `event "push" always runs everything` | only `pull_request` is narrowed |
| `no API key (set PICKY_API_KEY)` | add the secret, or set `model: none` |
| `no base commit to compare against (set PICKY_BASE)` | set `PICKY_BASE=<ref>` |
| `.picky.yml has unknown key "x"`, `` `extends` is not supported ``, `unknown mode`, `model "x" is not supported`, `.picky.yml is invalid` | fix `.picky.yml` |
| `internal error: ...` | picky failed open. Report the message. |
| `full run for js commands: package.json changed` | a lockfile, `package.json`, `tsconfig*.json`, `jsconfig.json` or vitest/vite/jest config runs JS in full. `composer.json`, `composer.lock` or `phpunit.xml` do the same for PHP; Gradle build files, `buildSrc/` or `includeBuild` dirs for Kotlin; `Cargo.toml`, `Cargo.lock` or `rust-toolchain*` for Rust. |
| `command not recognized; runs in full` | the marker is on an unsupported command. Remove it. |
| `no test files found under this directory; runs in full` | no test file of this runner's ecosystem under the command's directory. Check `working-directory`. |
| `model unavailable (...)`, `model gave no answer`, `diff too large for one request` | the tests the graph couldn't link run |
| `<tool> config <file> affected; runs in full` | ESLint or Prettier config changed |
| `<tool> before 10 runs only the first of several paths; runs in full` | PHPUnit 9 narrows only to one file |
| `Android and Kotlin Multiplatform test tasks take no --tests filter; runs in full` | a selected test is in an Android or KMP project |
| `N files <tool> checks read a changed file (e.g. X); runs in full` | a Rust or Kotlin source reads the changed file (`include_str!`, resources), so the check runs |

Per-test reasons: `critical test`, `test changed`, `imports <file>`, `model: possibly affected` (p >= 0.15), `model: unaffected`, `only low-risk files changed`, `no graph link (graph-only mode, model: none)`.

### Shadow miss

In shadow mode a marked command runs in full. If it fails, picky checks what enforce mode would have done:

| Enforce mode would have... | Result |
|---|---|
| skipped it | `picky shadow miss` |
| narrowed it, and the narrowed run passes | `picky shadow miss` |
| narrowed it, and the narrowed run fails too | caught: no miss |

A miss is a warning on the run and a section in the job summary. It never changes the step's result. A real miss means picky would have hidden a real failure. Do not enable `enforce`. Instead:

- Add the test or the source it depends on to `critical` in `.picky.yml`.
- With `model: none`, ask the user about `model: jev`: the model judges tests the graph can't link.
- Keep shadow mode on and watch the next PRs.

## Reference

### `.picky.yml`

All keys are optional. An unknown key or `extends` makes every run full.

| Key | Values | Default |
|---|---|---|
| `mode` | `shadow` (decide, log, run everything), `enforce` | `shadow` |
| `model` | `jev`, `typesafe/<jev-version>`, `none` (graph only) | `jev` |
| `critical` | globs. A change here, or a test here, always runs. | `[]` |
| `low` | globs. Changes here never select tests on their own. | `["**/*.md", "**/*.mdx", "docs/**", "LICENSE*"]` |

### Environment variables

| Variable | Use |
|---|---|
| `PICKY_API_KEY` | model API key (`TYPESAFE_API_KEY` also works) |
| `PICKY_MODE`, `PICKY_MODEL` | override `mode` and `model` |
| `PICKY_BASE` | compare against this ref instead of the PR base |
| `PICKY_BUDGET_SECONDS` | model time budget, default 30 |

### Commands

| Command | Use |
|---|---|
| `picky init [--write]` | show (or apply) the files and workflow edits |
| `picky explain` | print the decision for this branch |
| `picky decide` | compute the decision once, for other jobs to reuse (the `picky` job runs it) |
| `picky -- <command...>` | run a command narrowed to the change |
| `picky --version` | print the version |

### Supported tools

Recognized when run directly (`vendor/bin/phpunit`), through `npx`, `bunx`, `pnpm|yarn|bun exec` or `php`, and through `npm`/`pnpm`/`yarn` (`test`, `run <script>`) and `composer` scripts when the script is one recognized command.

| Tool | What picky does |
|---|---|
| vitest, jest, `node --test`, PHPUnit, Pest | runs only the affected test files |
| `gradle test`, `./gradlew test` | runs only the affected test classes: `:proj:test --tests 'pkg.Class*'` per project |
| `cargo test`, `cargo nextest run` | runs only the affected crates (`-p`) and integration targets (`--test`) |
| ktlint (no patterns) | checks only changed `.kt`/`.kts` files |
| ESLint (flat config) | lints changed files and every file that imports them |
| Prettier | checks only the changed files |
| tsc, vue-tsc, svelte-check, biome | skips when no changed file can matter, else runs in full |
| phpstan, psalm, phpcs, php-cs-fixer, pint, rector | skips when no changed file can matter, else runs in full |
| detekt, `gradle check`/`ktlintCheck`/`spotlessCheck`/`lint`/`compileKotlin`, `cargo clippy`/`check`/`fmt` | skips when no changed file can matter, else runs in full |

- A test file is `*.test.*` or `*.spec.*` (ts, tsx, js, jsx, mts, cts, mjs, cjs), or `*Test.php`; a test class under `src/test/kotlin|java/`; a Rust crate (unit tests and doctests) or integration target `tests/<name>.rs`. `node_modules/` and `vendor/` are ignored.
- Legacy `.eslintrc`, `eslint -c ...` and `import/no-unused-modules`: skip or run in full, never narrowed.
- `php artisan test` always runs in full. PHPUnit 9 is narrowed only to a single file.
- Gradle: only a bare `test` task is narrowed. Android, Kotlin Multiplatform and Maven (`mvn test`) run in full.
- Cargo: `-p`, `--lib`, `--test <name>` or a name filter run in full. Narrowed nextest runs add `--no-tests=pass`, which older nextest versions reject.
- Any other marked command runs in full.

### Not supported yet

Go, Maven, other model providers, `extends` presets, `cmd` and custom shells in `shell: picky ...`, and CI systems other than GitHub Actions (`init` only edits `.github/workflows/`). Self-hosted runners and GitHub Enterprise Server are not verified.
