# Changelog

All notable changes to picky are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0-rc] - 2026-10-18

The first public release candidate.

### Added

- **Selection for JS/TS and PHP.** A file-level import graph for JS/TS (relative imports, tsconfig `paths`, workspaces, dynamic imports, re-exports) and a class-level graph for PHP (PSR-4, classmaps, `use`, docblocks, `require`, Pest and PHPUnit bootstraps). Tests that use a deleted file or class are selected.
- **Jev judge** for tests the graph can't link: one question per test, kept at p ≥ 0.15, retried within a 30 s budget. Any failure runs the test.
- **Full-run gates:** `.picky.yml`, workflows and `critical` paths run everything; lockfiles and runner configs run their ecosystem in full; non-PR events run everything.
- **Narrowed commands:** vitest, jest, `node --test`, PHPUnit and Pest run only affected test files; ESLint lints changed files and their importers; Prettier checks changed files; tsc, phpstan, psalm, biome and other checkers skip when nothing relevant changed.
- **`# picky` marker** through a `BASH_ENV` hook, and `shell: picky {0}` for Windows, container and `sh`/`pwsh` steps.
- **Job-level setup skip:** `npm ci`, `composer install` and other installs are skipped when every marked command in the job skips.
- **`picky init`:** sets up a repo in one command, shown as a diff first, and vendors a single-file bundle so CI downloads nothing at run time.
- **`picky decide`:** one decision per PR, shared across jobs and matrices.
- **Shadow mode** (the default): runs everything, logs what picky would have done, and reports a `picky shadow miss` when enforce mode would have missed a failure.
- `picky --version`.

[Unreleased]: https://github.com/ahmedragab20/picky/compare/v0.1.0-rc...HEAD
[0.1.0-rc]: https://github.com/ahmedragab20/picky/releases/tag/v0.1.0-rc
