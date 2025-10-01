# Repository Guidelines

## Project Structure & Module Organization

cargo-hold is a Rust-based CI helper focused on stabilizing incremental builds. Core sources live in `src/`, with `cli.rs` wiring clap subcommands (anchor, salvage, stow, bilge, heave, voyage). State tracking flows through `state.rs`, `metadata.rs`, and `hashing.rs`, while `discovery.rs` filters to Git-tracked files. Timestamp and lifecycle logic sit in `timestamp.rs` and `gc.rs`, and errors consolidate in `error.rs`. Integration tests under `tests/` exercise full command workflows, and metadata persists to `target/cargo-hold.metadata`.

## Build, Test, and Development Commands

- `cargo build` / `cargo build --release`: Compile debug or optimized binaries.
- `cargo nextest run` or `cargo nextest run --profile ci`: Preferred fast, fail-safe test execution.
- `cargo check --all-targets`: Type-check the workspace without linking.
- `cargo clippy --all-targets -- -D warnings`: Enforce lint-clean code.
- `cargo +nightly fmt` and `cargo +nightly fmt --check`: Apply or verify formatting.
- `cargo deny check` and `cargo audit`: Guard licenses and security issues.
- `cargo hold voyage`: Run the end-to-end CI flow locally before pushing.

## Coding Style & Naming Conventions

Use Rust 2024 formatting via `cargo +nightly fmt`; do not hand-edit spacing. Keep modules and files snake_case, types UpperCamelCase, and constants SCREAMING_SNAKE_CASE. Lean on `thiserror` + `miette` for diagnostics, avoid `unwrap()`, and prefer `.wrap_err()` when adding context. Document non-obvious code paths with brief comments only where needed.

## Testing Guidelines

Embed focused unit tests in source `#[cfg(test)]` blocks and create integration scenarios in `tests/` with temporary workspaces. Name tests by behavior (e.g., `anchor_restores_stale_timestamps`). Default to `cargo nextest run`; scope with `-E 'test(pattern)'` for targeted runs. Keep slow or flaky cases behind `#[ignore]` and justify them in the test doc comment.

## Commit & Pull Request Guidelines

Follow Conventional Commits (e.g., `feat: add salvage dry-run`). Each PR should describe motivation, summarize changes, and link relevant issues. Include output snippets for key commands (`cargo nextest run`, `cargo clippy`). Ensure CI passes, request reviews for riskier timestamp or metadata changes, and update docs when behavior shifts.

## CI & Operational Tips

GitHub Actions runs build, lint, audit, deny, and test matrices across Ubuntu and macOS. Cache hits depend on stable metadata, so avoid manual edits to `target/cargo-hold.metadata`. Minimum supported Rust is 1.88.0; regenerate metadata using `cargo hold anchor` after major dependency upgrades.
