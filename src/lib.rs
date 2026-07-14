//! bashprof — line-level time profiler for bash scripts.
//!
//! The pipeline is a straight line: [`ps4`] builds the trace instrumentation
//! (a machine-readable `PS4` plus a bootstrap that sources the target script
//! under `set -x` with `BASH_XTRACEFD` pointing at a trace file), [`runner`]
//! executes it, [`trace`] parses the raw trace into timestamped events,
//! [`profile`] turns events into per-line / per-function self-time, and
//! [`report`] / [`collapse`] / [`jsonout`] render the result.

pub mod cli;
pub mod collapse;
pub mod jsonout;
pub mod profile;
pub mod ps4;
pub mod report;
pub mod runner;
pub mod trace;

/// Crate version, single source of truth for `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
