# Contributing to picky

Thanks for helping. Bug reports, fixes and new tool recognizers are all welcome.

## Setup

```bash
git clone https://github.com/ahmedragab20/picky && cd picky
bun install          # Bun 1.4.3+ runs the tooling; Node 24+ runs the TypeScript source directly
bun run test         # bun test --check: type checks the tests and what they import, then runs them
bun run typecheck    # bun check, plus a Node-only check of src/
bun run demo         # end-to-end run through the CI hook
```

`bin/picky` runs the source on Node, the runtime users have. `bun run build` type checks `src/` and makes the single-file bundle `dist/picky.mjs`, which is what `picky init` vendors into repos. `src/tsconfig.json` leaves out Bun's types, so a Bun-only API in `src/` fails the check.

## Layout

| Path | What's there |
|---|---|
| `src/decide.ts` | the decision: gates, graph floor, model judge |
| `src/graph.ts`, `src/php.ts` | JS/TS import graph and PHP class graph |
| `src/commands.ts`, `src/plan.ts` | recognized commands and how each is narrowed |
| `src/hook.ts`, `hook/hook.sh`, `src/shell.ts` | the `BASH_ENV` hook and `shell: picky {0}` |
| `src/init.ts` | `picky init` |
| `bench/` | the selection benchmark behind the docs charts |
| `docs/` | the documentation site (Fumadocs, deployed to GitHub Pages) |

## Rules for changes

- **Fail open.** Any new path that can error must end in a full run, never a skipped test.
- **Test observable behavior.** A bug fix comes with a test that fails before the fix.
- **Keep it small.** No new runtime dependencies without an issue first.
- Run `bun run test` and `bun run typecheck` before opening a PR. CI runs both, plus the build.

## Commits and PRs

Commits follow [Conventional Commits](https://www.conventionalcommits.org/): `feat(php): resolve enum cases`, `fix(hook): keep pipefail`.

Add a line under `## [Unreleased]` in `CHANGELOG.md` for anything users would notice. The docs changelog page is built from that file.

## Docs

```bash
cd docs && bun install && bun run dev    # http://localhost:3000
```

Pages are in `docs/content/docs/*.mdx`. Keep them short: a table, a chart or a code block first, with details in an `<Accordion>`.

## Releases (maintainers)

1. Move the `Unreleased` entries under a new version heading in `CHANGELOG.md`.
2. Set the same version in `package.json`.
3. Tag and push: `git tag v<version> && git push origin v<version>`.

The release workflow tests, builds `picky.mjs`, and publishes it with a SHA-256 checksum. A tag with `-rc` or `-beta` is marked as a pre-release.

## Security

Report vulnerabilities through a [private security advisory](https://github.com/ahmedragab20/picky/security/advisories/new), not an issue.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
