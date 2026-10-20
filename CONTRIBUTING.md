# Contributing to picky

Thanks for helping. Bug reports, fixes and new tool recognizers are all welcome.

## Setup

```bash
git clone https://github.com/ahmedragab20/picky && cd picky
cargo test                                   # unit and integration tests (the hook tests need bash and node)
cargo clippy --all-targets -- -D warnings    # lint
cargo fmt --check                            # formatting (cargo fmt fixes it)
bash examples/demo.sh                        # end-to-end run through the CI hook
```

Rust 1.88+ (stable). `cargo build --release` makes `target/release/picky`, the single binary `picky init` vendors into repos. The release workflow builds it for Linux (static, musl), macOS and Windows. The docs site in `docs/` is the only JS left: Bun 1.4.3+ runs it, with oxlint and oxfmt (`cd docs && bun run lint`).

## Layout

| Path | What's there |
|---|---|
| `src/decide.rs` | the decision: gates, graph floor, model judge |
| `src/graph.rs`, `src/tsimports.rs`, `src/php.rs`, `src/kotlin.rs`, `src/rust.rs` | the import and class graphs per ecosystem |
| `src/commands.rs`, `src/plan.rs` | recognized commands and how each is narrowed |
| `src/hook.rs`, `hook/hook.sh`, `src/shell.rs` | the `BASH_ENV` hook and `shell: picky {0}` |
| `src/init.rs` | `picky init` |
| `src/yaml.rs`, `src/jsonc.rs`, `src/glob.rs`, `src/jsre.rs` | YAML, tsconfig JSON, globs and JS regexes, matching what the TypeScript version used (`yaml`, `typescript`, `picomatch`) |
| `tests/` | integration tests: real git repos, the real hook under bash |
| `bench/` | the selection benchmark behind the docs charts |
| `docs/` | the documentation site (Fumadocs, deployed to GitHub Pages) |

## Rules for changes

- **Fail open.** Any new path that can error must end in a full run, never a skipped test.
- **Test observable behavior.** A bug fix comes with a test that fails before the fix.
- **Keep it small.** No new runtime dependencies without an issue first.
- Run `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` before opening a PR. CI runs all three on Linux, macOS and Windows.

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
2. Set the same version in `Cargo.toml` (and run `cargo build` so `Cargo.lock` follows).
3. Tag and push: `git tag v<version> && git push origin v<version>`.

The release workflow tests, builds a binary per platform (`picky-linux-x64`, `picky-linux-arm64`, `picky-darwin-x64`, `picky-darwin-arm64`, `picky-windows-x64.exe`), and publishes them with a `SHA256SUMS` file. `picky init` downloads the binaries from that release and checks them against `SHA256SUMS`, so publish every asset before announcing a version. A tag with `-rc` or `-beta` is marked as a pre-release.

## Security

Report vulnerabilities through a [private security advisory](https://github.com/ahmedragab20/picky/security/advisories/new), not an issue.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
