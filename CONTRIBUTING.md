# Contributing

Thanks for your interest in contributing! This guide explains how to set up your environment, make changes, run checks locally, and submit high‑quality pull requests that pass CI.

---

## Project Overview

`event-scanner` is a Rust library for monitoring and streaming EVM-based smart contract events. It is built on the `alloy` ecosystem and provides in-memory scanning. See `README.md` for features, usage, examples, and testing notes.

- Workspace manifest: `Cargo.toml`
- Library code: `src/`
- Examples: `examples/simple_counter`, `examples/historical_scanning`
- Integration tests: `tests/`
- Formatting: `rustfmt.toml`
- Toolchain pin: `rust-toolchain.toml`
- CI: `.github/workflows/check.yml` and `.github/workflows/test.yml`

---

## Prerequisites

- Rust toolchains via `rustup`
  - Minimum supported Rust version (MSRV): 1.89 (see `rust-toolchain.toml`)
  - Formatting uses nightly `rustfmt` in CI
- Recommended tooling
  - `cargo-nextest` for fast, reliable test runs: `cargo install cargo-nextest`
  - `typos` for spell checking: `cargo install typos-cli` (or `brew install typos`)
  - A recent `rust-analyzer` for IDE support
- Runtime/dev tools
  - For examples and some tests, you’ll need an Ethereum dev node such as Foundry’s `anvil`

---

## Repository Layout

- `src/` – core library
- `examples/` – runnable examples that deploy a demo contract and scan events
- `tests/` – integration tests covering live, historical and hybrid flows
- `.github/workflows/` – CI configuration (build, fmt, clippy, typos, tests)

---

## Getting Started

1. Fork the repository and clone your fork.
2. Ensure toolchains are installed:
   - `rustup toolchain install 1.89` (MSRV)
   - `rustup toolchain install nightly` (for rustfmt parity with CI)
3. Optional helpers:
   - `cargo install cargo-nextest`
   - `cargo install typos-cli`

Build the workspace:

```bash
cargo build --locked --all-targets --all-features
```

Run the test suite (we recommend nextest):

```bash
cargo nextest run
# or
cargo test
```

Run examples:

```bash
RUST_LOG=info cargo run -p simple_counter
# or
RUST_LOG=info cargo run -p historical_scanning
```

Note: Examples start a local `anvil` instance and deploy a demo contract.

---

## Code Style & Linting

- Formatting is enforced by `rustfmt` with the settings in `rustfmt.toml`.
  - CI runs formatting on nightly. To match CI locally:
    ```bash
    cargo +nightly fmt --all --check
    ```
  - To apply formatting locally:
    ```bash
    cargo +nightly fmt --all
    ```
- Linting is enforced with `clippy` and pedantic lints are denied.
  - To check locally (matches CI):
    ```bash
    cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic
    ```
  - To apply automatic fixes where possible:
    ```bash
    cargo clippy --all-targets --all-features --fix --allow-dirty --allow-staged -- -D warnings -D clippy::pedantic
    ```
- Spelling/typos:
  - CI uses `typos`. Locally:
    ```bash
    typos
    ```

---

## Local CI Parity Checklist

Before opening a PR, please ensure the following commands succeed locally:

```bash
# Build (all targets & features)
cargo build --locked --all-targets --all-features

# Format (nightly to match CI)
cargo +nightly fmt --all --check

# Lint (deny warnings, pedantic)
cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic

# Spelling
typos

# Tests (prefer nextest)
cargo nextest run
```

---

## Branching, Commits, and PRs

- Branch from `main` with a descriptive name, e.g. `feat/reorg-handling`, `fix/rpc-retries`, `docs/contributing`.
- Keep PRs focused and reasonably small; large changes are harder to review.
- Write clear commit messages that explain the why and the what. Conventional Commits are welcome but not required.
- Include tests for new functionality or bug fixes where applicable.
- Update documentation and examples as needed.
- Ensure CI passes. Required checks include:
  - Build: cargo build
  - Format: rustfmt (nightly)
  - Lint: clippy (pedantic, warnings denied)
  - Typos: typos
  - Tests: see `.github/workflows/test.yml`
- CODEOWNERS: reviewers may be auto-assigned based on `CODEOWNERS`.

PR description template (suggested):

```markdown
## Summary
Short summary of the change and context.

## Motivation
Why is this change needed?

## Approach
Key implementation details, trade-offs, and alternatives considered.

## Checklist
- [ ] Builds locally with `--all-targets --all-features`
- [ ] `cargo +nightly fmt --all --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings -D clippy::pedantic`
- [ ] `typos`
- [ ] Tests pass (`cargo nextest run`)
- [ ] Docs/README/examples updated (if applicable)
```

---

## Testing Notes

- Integration tests live under `tests/` and cover live, historical, and hybrid flows.
- Prefer `cargo-nextest` for speed and improved output.
- Where practical, add regression tests when fixing bugs.

---

## Design & Architecture

- High-level architecture and core traits/components are described in `README.md`.
- For non-trivial changes (new abstractions, public API changes), please open an issue first to discuss design direction before implementation.

---

## Filing Issues

- Use GitHub Issues for bug reports and feature requests.
- Include reproduction steps, expected vs actual behavior, logs, and environment details.
- Security-related reports: please follow `SECURITY.md`.

---

## Release & Versioning

- Public crates follow semver. Breaking changes to the public API should be noted clearly in PRs and release notes.
- New features and fixes may be released in pre-releases before a stable cut.

---

## Community Standards

- Be respectful and constructive.
- Prefer clear, actionable feedback.
- Assume good intent and collaborate to reach the best solution.

Thanks again for contributing to Event Scanner!
