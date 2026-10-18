# picky

[![CI](https://github.com/ahmedragab20/picky/actions/workflows/ci.yml/badge.svg)](https://github.com/ahmedragab20/picky/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-c4a7e7)](LICENSE)

**Run only the tests your PR can affect.** picky reads the diff, follows the import graph, and runs only the tests, lint and type checks a change can reach, in GitHub Actions, for JS/TS and PHP. Any doubt runs everything.

[Docs](https://ahmedragab20.github.io/picky/) · [Benchmarks](https://ahmedragab20.github.io/picky/docs/benchmarks/) · [For agents](https://ahmedragab20.github.io/picky/docs/agents/) · [Changelog](CHANGELOG.md)

## Benchmarks

The last 100 commits of 4 open-source repos, each replayed as a PR through picky in graph-only mode (`model: none`, no cache):

| Repo | Test files | Test files skipped (avg) | PRs that ran no test | Time to decide (p50) |
|---|---:|---:|---:|---:|
| [colinhacks/zod](https://github.com/colinhacks/zod) | 202 | **72%** | 32 of 100 | 108 ms |
| [honojs/hono](https://github.com/honojs/hono) | 160 | **45%** | 4 of 100 | 93 ms |
| [composer/composer](https://github.com/composer/composer) | 186 | 13% | 12 of 100 | 216 ms |
| [laravel/framework](https://github.com/laravel/framework) | 1,008 | 8% | 5 of 100 | 748 ms |

TypeScript repos split cleanly along imports. In the two PHP repos, the base `TestCase` imports the whole app, so the class graph reaches almost every test; symbol-level analysis is next. These are graph-only numbers: with the Jev model on, picky can only add tests. Method and per-commit data: [`bench/`](bench/) and the [benchmarks page](https://ahmedragab20.github.io/picky/docs/benchmarks/).

## Install

```bash
curl -fsSLO https://github.com/ahmedragab20/picky/releases/download/v0.1.0-rc/picky.mjs
node picky.mjs init           # prints every change as a diff
node picky.mjs init --write   # applies it
rm picky.mjs
```

Then add the `PICKY_API_KEY` repository secret (or set `model: none`), commit, and open a PR. Needs Node 20+.

- **Vendored:** `init` puts one 3.8 MB file in `.github/picky/`. CI downloads nothing, and git pins the version.
- **Safe rollout:** it starts in shadow mode. Everything still runs, and picky reports any failure enforce mode would have missed.
- **Enterprise:** checksum-verified downloads, no third-party actions, and a zero-egress mode. See [Install](https://ahmedragab20.github.io/picky/docs/install/).
- **With a coding agent:** paste [one prompt](https://ahmedragab20.github.io/picky/docs/agents/) into Claude Code, Cursor or Codex, or install the [picky skill](skills/picky/SKILL.md).

## Where it pays off

- **Docs, assets and CI-script PRs:** no test can see the change, so tests, type checks and the `npm ci` before them all skip.
- **Big JS/TS suites:** a fix in one module runs the tests that import it. Vitest, Jest, `node --test`, ESLint, Prettier.
- **PHP projects:** PHPUnit and Pest narrowed by the class graph; phpstan, psalm, phpcs, pint and rector skip when nothing they check changed.
- **Matrix and multi-OS jobs:** one decision per PR, shared by every job on Linux, Windows and containers.

## How it decides

1. **Diff** against the PR base.
2. **Gates:** a change to `.picky.yml`, workflows or `critical` paths runs everything. A lockfile or `package.json` change runs that ecosystem in full.
3. **Graph:** every test that reaches a changed file through imports runs.
4. **Jev:** a model judges the tests the graph can't link. Unsure means run.
5. **Run:** each command marked `# picky` is skipped, narrowed or run in full.

Any error, timeout, missing key or config picky can't honor runs everything. If picky itself breaks, the original command runs.

```yaml
# .picky.yml (every key is optional)
mode: shadow           # shadow | enforce
model: jev             # jev | none (graph only, no network)
critical: ["db/**"]    # a change here always runs everything
low: ["docs/**"]       # changes here never select tests on their own
```

## Try it locally

```bash
git clone https://github.com/ahmedragab20/picky && cd picky
npm install && npm run demo
```

The demo builds a small repo and runs 4 PRs through the hook: a source change, a docs-only change, a `package.json` change and shadow mode.

## Limits

- GitHub Actions only, on `pull_request` events. JS/TS and PHP only.
- Verified on GitHub-hosted runners. Self-hosted runners and GitHub Enterprise Server are not verified yet.
- More in [Limits](https://ahmedragab20.github.io/picky/docs/limits/).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under [MIT](LICENSE).
