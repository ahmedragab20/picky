# picky

Runs only the tests, lint and type checks a change can affect, in JS/TS and PHP.
- A dependency graph decides what must run.
- A model (Jev) judges the tests the graph can't link.
- Any doubt, error or timeout means everything runs.

Status: **v0.2, a working slice of [PLAN.md](PLAN.md)** (see its "Build status" section). It has been verified end to end on GitHub-hosted runners with a private fixture repo.

## Try it

```bash
npm install
npm run demo                              # graph-only, no API key needed
PICKY_API_KEY=<typesafe key> npm run demo # adds Jev for tests outside the graph
```

The demo builds a small TypeScript repo and runs a CI step through the picky hook, exactly the way GitHub Actions runs `run:` steps. It covers 4 scenarios: a source change, a docs-only change, a `package.json` change and shadow mode.

## Use it in a repo

```yaml
# .picky.yml (every key is optional)
mode: enforce          # shadow (default): decide and log, but run everything
model: jev             # jev | typesafe/<jev-version> | none (graph only)
critical: ["db/**", "test/security/**"]   # a change here, or a test here, always runs
low: ["docs/**", "**/*.md"]               # changes here never trigger tests
```

```bash
export PICKY_API_KEY=...                 # TYPESAFE_API_KEY also works
bin/picky explain                        # show the decision for this branch
bin/picky -- npx vitest run              # run narrowed to the affected test files
```

### In a CI step: the `# picky` marker

```yaml
env:
  BASH_ENV: ${{ github.workspace }}/.github/picky/hook.sh   # copy of hook/hook.sh
  PICKY_API_KEY: ${{ secrets.PICKY_API_KEY }}
steps:
  - uses: actions/checkout@v4
  - run: npm ci                     # skipped when every marked step below skips
  - run: npx eslint .        # picky
  - run: npx vitest run      # picky
  - run: |
      vendor/bin/phpunit     # picky
```

A marker works as the YAML comment of a one-line `run:` and inside a block script. YAML drops the one-line comment, so picky reads it from the workflow file.

Bash sources the hook before every step:
- **Steps without a marker or an install command:** run untouched.
- **Marked lines** are skipped, narrowed or run in full:

| Command | What picky does |
|---|---|
| vitest, jest, `node --test`, PHPUnit, Pest | runs only the affected test files |
| ESLint (flat config) | lints the changed files and every file that imports them |
| Prettier | checks only the changed files |
| tsc, vue-tsc, svelte-check, biome, phpstan, psalm, phpcs, php-cs-fixer, pint, rector | skips when no changed file can matter to it; otherwise runs in full |

  These commands are also recognized through npm/pnpm/yarn and composer scripts.
- **Install commands** (`npm ci`, `pnpm install`, `composer install`, `npx playwright install`, …) are skipped when every other command in the job is a marked command that skips.
- **If picky fails**, the original line runs.

### Several jobs: one decision per PR

Model answers vary slightly between calls, so jobs that decide on their own can disagree about a test near the threshold. Compute the decision once and share it:

```yaml
jobs:
  picky:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: .github/picky/picky decide          # always exits 0
      - uses: actions/upload-artifact@v4
        with: { name: picky-decision, path: "${{ runner.temp }}/picky", retention-days: 7 }
  test:
    needs: picky
    if: ${{ !cancelled() }}                       # runs even if the picky job failed
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        continue-on-error: true                   # without it, the job decides on its own
        with: { name: picky-decision, path: "${{ runner.temp }}/picky" }
      - run: npx vitest run  # picky
```

The model is called once per PR, and every job acts on the same answers. The cost is the `picky` job, about 10 s before the other jobs start.

## What v0.2 does

- **Diff:** merge-base diff against `main`, a PR's base, or `PICKY_BASE`, with renames and deletions.
- **Full-run gates:**
  - `.picky.yml`, workflows or `critical` paths changed: everything runs.
  - A JS lockfile, `package.json`, `tsconfig` or runner config changed: JS commands run in full.
  - `composer.json`, `composer.lock` or `phpunit.xml` changed: PHP commands run in full.
  - Any non-PR event in Actions, a missing key, or config picky can't honor (e.g. `extends`): everything runs.
- **JS/TS import graph:** relative imports, `.js` → `.ts`, index files, tsconfig `paths`, workspace packages, dynamic imports and re-exports. Tests that still import a deleted file are selected.
- **PHP class graph:**
  - Every declared class and function is indexed, so PSR-4, classmaps and custom autoloaders all resolve.
  - It follows `use` (aliases, groups), same-namespace names, class names in docblocks and strings, and `require`/`include`.
  - `Pest.php` and the `phpunit.xml` bootstrap link to every test under them.
  - Users of a deleted class are selected.
  - PHPUnit 9 ignores all but the first path, so it's only narrowed to a single file.
- **Jev judge:**
  - One `noul` question per test outside the graph floor; a test is kept if p ≥ 0.15.
  - The model sees the full zero-context diff.
  - On 429/5xx it retries within a 30s budget; on failure, a missing answer or an oversized diff, the test runs.
- **Decision reuse and summary:** one decision per commit, cached and shared across steps, and across jobs with `picky decide`. It's printed to the log and to the GitHub job summary.

## Known limits

- **Code generated during the job:** a step that generates code from files picky considers irrelevant (e.g. TS types generated from PHP classes) can make a skipped type check wrong. Put those source files under `critical`.
- **Graph-only mode (`model: none`):** it skips tests the graph can't link, such as Laravel feature tests that depend only on a route file. With Jev, those tests are judged.

## Not yet (see PLAN.md "Build status")

- **Not in v0.2:**
  - binary distribution for GitHub (the hook needs `PICKY_BIN` or `picky` on `PATH`)
  - `picky init`
  - a shadow-mode recall report
  - `extends` presets
  - other model providers
  - Go and Rust
  - symbol-level analysis
  - context packing for huge diffs
  - history and coverage
  - `php artisan test` (it runs in full)
- **Verified live:** Jev on GitHub runners, about 700 input tokens and 200–400 ms per PR in the fixture repo.
