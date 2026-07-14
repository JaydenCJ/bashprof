//! Trace instrumentation: the `PS4` prompt and the bash bootstrap.
//!
//! bash's xtrace mode (`set -x`) prints every simple command, prefixed by the
//! expansion of `PS4`. Two properties make it a viable profiler backend:
//!
//! 1. `PS4` undergoes parameter expansion at trace time, so it can embed
//!    `$EPOCHREALTIME` (microsecond wall clock, bash >= 5.0), `$BASH_SOURCE`,
//!    `$LINENO` and `$FUNCNAME` — a timestamped source location per command.
//! 2. `BASH_XTRACEFD` routes the trace to a private file descriptor, so the
//!    script's own stderr stays untouched.
//!
//! Fields are separated by ASCII unit separator (0x1F), a byte that never
//! appears in timestamps, line numbers or sane paths, and that bash's xtrace
//! quoting never emits on its own.

/// ASCII unit separator: the field delimiter inside trace lines.
pub const FIELD_SEP: char = '\u{1f}';

/// Environment variable through which the bootstrap learns the trace path.
pub const TRACE_ENV: &str = "BASHPROF_TRACE";

/// Prefix used by the bootstrap's internal function and variables so the
/// parser can filter them out of the profile.
pub const INTERNAL_PREFIX: &str = "__bashprof";

/// File descriptor the bootstrap dedicates to the trace stream.
pub const TRACE_FD: u32 = 9;

/// Build the `PS4` value.
///
/// Layout (after the leading `+`, which bash repeats once per subshell level):
/// `US ts US source US lineno US funcname US funcdepth US` — the traced
/// command follows the final separator. We record `${FUNCNAME[0]}` (current
/// function) plus the frame count instead of the whole `${FUNCNAME[*]}`
/// stack: joining the array uses the first character of `IFS`, which
/// "strict mode" scripts commonly set to `\n\t` — that would split trace
/// lines. Name + depth is IFS-proof and lets the profiler reconstruct the
/// full stack deterministically.
///
/// The frame count uses the `${FUNCNAME[@]+...}` guard: `FUNCNAME` is
/// *unset* (not empty) at the top level of a sourced script, and a bare
/// `${#FUNCNAME[@]}` would abort `set -u` scripts the moment tracing
/// expands it. Unset expands to nothing; the parser reads that as depth 0.
pub fn ps4_value() -> String {
    let s = FIELD_SEP;
    format!(
        "+{s}${{EPOCHREALTIME}}{s}${{BASH_SOURCE[0]:-}}{s}${{LINENO}}{s}${{FUNCNAME[0]:-}}{s}${{FUNCNAME[@]+${{#FUNCNAME[@]}}}}{s}"
    )
}

/// Build the bootstrap program passed to `bash -c`.
///
/// It is invoked as `bash -c <bootstrap> <script> [args...]`, so `$0` is the
/// target script path and `$@` are its arguments. The bootstrap:
///
/// 1. opens the trace file on fd 9 and points `BASH_XTRACEFD` at it,
/// 2. assigns `PS4` *inside the shell* — bash (since 5.0) refuses to import
///    `PS4` from the environment when running as root, so passing it via
///    `env` would silently produce an unparseable trace in containers and
///    CI runners,
/// 3. writes a `START` control record with the wall clock and script path,
/// 4. installs an EXIT trap that writes an `END` record with the exit code
///    (this also stamps the *last* line's duration, and survives `exit`
///    anywhere in the script),
/// 5. enables `set -x` and *sources* the script, so `$0`, positional
///    parameters and `exit` behave exactly as in a direct run.
pub fn bootstrap() -> String {
    let sep = FIELD_SEP;
    // ps4_value() contains no single quotes, so single-quoting is safe and
    // keeps the ${...} expansions for trace time.
    format!(
        r#"{p}_finish() {{
  local {p}_rc=$?
  {{ set +x; }} 2>/dev/null
  printf '{sep}END{sep}%s{sep}%s\n' "$EPOCHREALTIME" "${p}_rc" >&{fd} 2>/dev/null || true
  exit "${p}_rc"
}}
exec {fd}>>"${env}" || exit 127
BASH_XTRACEFD={fd}
PS4='{ps4}'
unset {env}
printf '{sep}START{sep}%s{sep}%s\n' "$EPOCHREALTIME" "$0" >&{fd}
trap {p}_finish EXIT
set -x
. "$0"
"#,
        p = INTERNAL_PREFIX,
        sep = sep,
        fd = TRACE_FD,
        env = TRACE_ENV,
        ps4 = ps4_value(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps4_layout_plus_prefix_five_fields_trailing_separator() {
        // bash repeats the *first* character of PS4 once per nesting level;
        // depth detection depends on that character being '+'. The traced
        // command is appended right after the final separator.
        let ps4 = ps4_value();
        assert!(ps4.starts_with('+'));
        assert!(ps4.ends_with(FIELD_SEP));
        let idx = |needle: &str| {
            ps4.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        };
        let ts = idx("${EPOCHREALTIME}");
        let src = idx("${BASH_SOURCE[0]:-}");
        let line = idx("${LINENO}");
        let func = idx("${FUNCNAME[0]:-}");
        let depth = idx("${FUNCNAME[@]+${#FUNCNAME[@]}}");
        assert!(ts < src && src < line && line < func && func < depth);
    }

    #[test]
    fn ps4_is_ifs_proof_and_set_u_safe() {
        // ${FUNCNAME[*]} joins with IFS[0]; strict-mode scripts set
        // IFS=$'\n\t' which would break line-based parsing — it must never
        // come back. And FUNCNAME is *unset* at the top level of a sourced
        // script, so every expansion touching it needs a `:-` or `[@]+`
        // guard or `set -u` scripts die on their first traced command.
        let ps4 = ps4_value();
        assert!(!ps4.contains("FUNCNAME[*]"));
        assert!(ps4.contains("${FUNCNAME[0]:-}"));
        assert!(ps4.contains("${FUNCNAME[@]+"));
    }

    #[test]
    fn bootstrap_sources_the_script_traps_exit_and_fails_loudly() {
        let b = bootstrap();
        assert!(b.contains(". \"$0\""), "must source, not exec, the script");
        assert!(b.contains("trap __bashprof_finish EXIT"));
        assert!(b.contains("set -x"));
        assert!(b.contains("BASH_XTRACEFD=9"));
        // An unopenable trace file must abort with a distinctive code.
        assert!(b.contains("|| exit 127"));
    }

    #[test]
    fn bootstrap_unsets_the_trace_env_before_running_user_code() {
        // The profiled script must not observe (or inherit into children)
        // bashprof's internal environment.
        let b = bootstrap();
        let unset = b.find("unset BASHPROF_TRACE").expect("must unset");
        let set_x = b.find("set -x").expect("must set -x");
        assert!(unset < set_x);
    }

    #[test]
    fn bootstrap_assigns_ps4_in_shell_not_via_environment() {
        // bash >= 5.0 ignores an environment-inherited PS4 when euid is 0;
        // the assignment must live inside the bootstrap or profiling breaks
        // for root (containers, CI). Single quotes defer the expansions —
        // which requires the PS4 value itself to stay quote-free.
        let b = bootstrap();
        assert!(!ps4_value().contains('\''));
        assert!(b.contains(&format!("PS4='{}'", ps4_value())));
    }
}
