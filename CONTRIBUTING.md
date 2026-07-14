# Contributing to bashprof

Thanks for your interest in improving bashprof. Issues, discussions and pull requests are all welcome.

## Getting started

Prerequisites: Rust 1.75 or newer (stable toolchain), and bash 5.0+ on the machine (needed by the integration tests and the smoke test; the profiler itself requires it at runtime).

```bash
git clone https://github.com/JaydenCJ/bashprof.git
cd bashprof
cargo build
cargo test
bash scripts/smoke.sh
```

`scripts/smoke.sh` builds the binary, profiles a real script end to end and asserts on the report, the collapsed stacks, the JSON output, the annotation and exit-code passthrough. It finishes in well under a minute and must print `SMOKE OK`.

## Before you open a pull request

1. `cargo fmt` — formatting is enforced.
2. `cargo clippy --all-targets -- -D warnings` — clippy must be clean.
3. `cargo test` — unit tests and the CLI integration tests must pass.
4. `bash scripts/smoke.sh` — the smoke test must print `SMOKE OK`.
5. Add tests for behavior changes. Parsing and attribution logic lives in pure modules (`trace`, `profile`, `collapse`, `report`) that are easy to unit-test; please keep it that way. Anything that changes what the bootstrap injects into bash (`ps4`) needs an integration test against a real script.

## Ground rules

- Keep dependencies at zero. bashprof is std-only by design; adding a crate needs a very strong justification in the PR description.
- No network calls, ever, and no telemetry. The profiler reads a script, runs it, and writes local files — nothing else.
- Code comments and doc comments are written in English.
- The profiled script's behavior is sacred: it must keep its own `$0`, arguments, stdin/stdout/stderr, environment and exit code. Instrumentation changes that leak state into the profiled script are bugs, not trade-offs.
- Timing assertions in tests are forbidden; assert on counts, line numbers and exit codes, or use synthetic traces with fixed timestamps.

## Reporting bugs

Please include your `bashprof --version` and `bash --version` output, the exact command line, and — if you can share it — the raw trace from `--out` (or a minimal script that reproduces the issue). Attribution bugs are much easier to fix with a concrete trace ("line X shows N counts, expected M").

## Security

bashprof executes the script you point it at, with your privileges — treat untrusted scripts accordingly. If you find a security issue in bashprof itself (e.g. injection via crafted paths or trace content), please do not open a public issue; use GitHub's private vulnerability reporting on this repository instead.
