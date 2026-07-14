//! Execute a script under trace instrumentation.
//!
//! The profiled script keeps its own stdin/stdout/stderr (they are
//! inherited), its own `$0` and positional parameters, and its own exit
//! code. The only observable differences from an uninstrumented run are
//! file descriptor 9 (the trace stream) and `set -x` being enabled — which
//! writes to fd 9, not stderr, so even scripts that inspect their stderr
//! behave normally.

use crate::ps4;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Everything needed to launch a profiled run.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// The bash binary to use ("bash" resolved via PATH by default).
    pub shell: String,
    /// Target script path.
    pub script: PathBuf,
    /// Arguments passed through to the script.
    pub args: Vec<String>,
    /// Where the raw trace is written.
    pub trace_path: PathBuf,
}

/// Verify the chosen shell can profile: it must be bash with
/// `$EPOCHREALTIME` (bash >= 5.0). Returns the shell's version string on
/// failure so the error can name what was actually found.
pub fn probe_shell(shell: &str) -> Result<(), String> {
    let out = Command::new(shell)
        .arg("-c")
        .arg("if [ -n \"${EPOCHREALTIME:-}\" ]; then exit 0; fi; printf %s \"${BASH_VERSION:-not bash}\"; exit 1")
        .output()
        .map_err(|e| format!("cannot execute shell '{shell}': {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let found = String::from_utf8_lossy(&out.stdout);
    let found = if found.trim().is_empty() {
        "unknown".to_owned()
    } else {
        found.trim().to_owned()
    };
    Err(format!(
        "'{shell}' does not provide EPOCHREALTIME (need bash >= 5.0, found: {found})"
    ))
}

/// Build the `Command` for a profiled run. Split from [`run`] so the exact
/// invocation (arguments, environment) is unit-testable without spawning.
pub fn build_command(spec: &RunSpec) -> Command {
    let mut cmd = Command::new(&spec.shell);
    cmd.arg("--norc")
        .arg("-c")
        .arg(ps4::bootstrap())
        .arg(&spec.script) // becomes $0 inside the bootstrap
        .args(&spec.args)
        // PS4 itself is assigned inside the bootstrap (see ps4::bootstrap):
        // bash >= 5.0 ignores an environment PS4 when running as root.
        .env(ps4::TRACE_ENV, &spec.trace_path);
    cmd
}

/// Run the script under instrumentation and return its exit code.
///
/// A shell killed by a signal maps to `128 + signal`, matching bash's own
/// convention, so CI wrappers keep working.
pub fn run(spec: &RunSpec) -> Result<i32, String> {
    if !spec.script.is_file() {
        return Err(format!("script not found: {}", spec.script.display()));
    }
    let status = build_command(spec)
        .status()
        .map_err(|e| format!("failed to start '{}': {e}", spec.shell))?;
    match status.code() {
        Some(code) => Ok(code),
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                Ok(128 + status.signal().unwrap_or(1))
            }
            #[cfg(not(unix))]
            Ok(1)
        }
    }
}

/// Guarantee the trace ends with an `END` control record.
///
/// The bootstrap's EXIT trap normally writes it — but `trap` in bash is
/// last-writer-wins, so a profiled script that installs its own EXIT trap
/// silently replaces ours. `EPOCHREALTIME` and `SystemTime` read the same
/// wall clock, so appending the record from this side keeps totals and the
/// last line's duration honest (at worst it absorbs process teardown).
pub fn ensure_end_record(path: &Path, exit_code: i32) {
    use crate::ps4::FIELD_SEP;
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let has_end = content.lines().any(|l| {
        l.strip_prefix(FIELD_SEP)
            .is_some_and(|r| r.starts_with("END"))
    });
    if has_end {
        return;
    }
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let record = format!(
        "{sep}END{sep}{}.{:06}{sep}{exit_code}\n",
        now_us / 1_000_000,
        now_us % 1_000_000,
        sep = FIELD_SEP,
    );
    let _ = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut f| {
            use std::io::Write as _;
            f.write_all(record.as_bytes())
        });
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A collision-safe temp path for the raw trace when the user does not ask
/// to keep it (`--out`). Plain files under the OS temp dir; the caller
/// removes it after parsing.
pub fn temp_trace_path() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "bashprof-{}-{}-{}.trace",
        std::process::id(),
        nanos,
        n
    ))
}

/// Best-effort cleanup of a temp trace.
pub fn remove_temp(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> RunSpec {
        RunSpec {
            shell: "bash".into(),
            script: PathBuf::from("/tmp/x.sh"),
            args: vec!["--flag".into(), "value with space".into()],
            trace_path: PathBuf::from("/tmp/x.trace"),
        }
    }

    #[test]
    fn command_passes_script_as_dollar_zero_then_args() {
        let cmd = build_command(&spec());
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // --norc, -c, <bootstrap>, $0, script args...
        assert_eq!(args[0], "--norc");
        assert_eq!(args[1], "-c");
        assert_eq!(args[3], "/tmp/x.sh");
        assert_eq!(args[4], "--flag");
        assert_eq!(args[5], "value with space");
    }

    #[test]
    fn command_sets_trace_env_but_not_ps4() {
        let cmd = build_command(&spec());
        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(envs
            .iter()
            .any(|(k, v)| k == ps4::TRACE_ENV && v == "/tmp/x.trace"));
        // PS4 must be assigned inside the bootstrap, not exported: bash
        // discards an inherited PS4 for euid 0 (containers, CI runners).
        assert!(!envs.iter().any(|(k, _)| k == "PS4"));
    }

    #[test]
    fn missing_script_is_reported_before_spawning() {
        let mut s = spec();
        s.script = PathBuf::from("/nonexistent/definitely-missing.sh");
        let err = run(&s).unwrap_err();
        assert!(err.contains("script not found"));
    }

    #[test]
    fn probe_names_the_requirement_or_the_launch_failure() {
        // /bin/sh on many systems is dash/posix mode without EPOCHREALTIME;
        // if it *is* bash 5, probing succeeds — accept either, but an error
        // must name the requirement.
        if let Err(e) = probe_shell("/bin/sh") {
            assert!(e.contains("EPOCHREALTIME"));
        }
        let err = probe_shell("/nonexistent/not-a-shell").unwrap_err();
        assert!(err.contains("cannot execute"));
    }

    #[test]
    fn temp_trace_paths_are_unique() {
        let a = temp_trace_path();
        let b = temp_trace_path();
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("bashprof-"));
    }
}
