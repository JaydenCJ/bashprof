# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-07-13

### Added

- `bashprof run`: profile any bash script by wrapping it in a `PS4` + `BASH_XTRACEFD` bootstrap — the script keeps its own `$0`, arguments, stdin/stdout/stderr, exit code (passed through) and a clean stderr, since the trace goes to a private file descriptor.
- Hot-line report: per-line self-time, percentage, execution count, worst single execution (`max_us` in JSON), and the latest expanded command text; `--top`, `--sort self|count|line`, `--min-us` shape the table.
- Per-function section: call counts and self-time per function, reconstructed IFS-proof from `${FUNCNAME[0]}` + frame depth (survives `IFS=$'\n\t'` strict-mode scripts).
- `bashprof collapse`: flamegraph-ready collapsed stacks with `file:line` leaf frames, byte-stable output, directly consumable by `flamegraph.pl`, inferno or speedscope.
- `bashprof annotate`: source listing with per-line time and count in the gutter — doubles as a line-coverage view.
- `bashprof report --json`: stable machine-readable output with integer-microsecond times.
- Raw trace kept via `--out` and replayable offline by `report` / `collapse` / `annotate`; format documented in `docs/trace-format.md`.
- Robustness work the folklore one-liner never gets: locale-radix timestamps (`.` and `,`), `set -u`-safe PS4 expansions, root/CI-safe PS4 assignment (bash ignores an environment PS4 for euid 0), END-record backfill when the profiled script overrides the EXIT trap, subshell/background clamping, and tolerant parsing of garbled trace lines.
- Test suite: 70 unit tests, 17 CLI integration tests against the compiled binary, and `scripts/smoke.sh`.

[0.1.0]: https://github.com/JaydenCJ/bashprof/releases/tag/v0.1.0
