//! Raw trace parsing: xtrace lines -> timestamped [`Event`]s.
//!
//! A trace file contains three kinds of lines:
//!
//! * control records written by the bootstrap, starting with the field
//!   separator: `US START US <ts> US <script>` and `US END US <ts> US <rc>`;
//! * xtrace events: one or more `+` (subshell depth), then the five
//!   separator-delimited PS4 fields, then the command text;
//! * continuation lines: xtrace re-prints commands verbatim, so a command
//!   containing an embedded newline (heredoc-ish strings, multi-line
//!   `$'...'`) spills onto lines with no marker — they belong to the
//!   previous event.
//!
//! Parsing is tolerant by design: a profiler must not die because one line
//! in a multi-megabyte trace is garbled. Unparseable lines are counted, not
//! fatal.

use crate::ps4::{FIELD_SEP, INTERNAL_PREFIX};

/// One traced simple command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Subshell nesting level: number of leading `+` characters (1 = none).
    pub subshell: u32,
    /// Wall-clock timestamp in microseconds since the epoch.
    pub ts_us: u64,
    /// `${BASH_SOURCE[0]}` — the file the command came from.
    pub source: String,
    /// `${LINENO}` — 1-based line in `source`.
    pub line: u32,
    /// `${FUNCNAME[0]}` — current function, empty at top level.
    pub func: String,
    /// `FUNCNAME` frame count: 0 at the top level of the script (bash keeps
    /// `FUNCNAME` unset there, which the PS4 renders as an empty field);
    /// inside a function the count includes the implicit `source` frame the
    /// bootstrap adds, so the first real function level arrives as 2.
    pub func_depth: u32,
    /// The command text as xtrace printed it.
    pub command: String,
}

/// A fully parsed trace.
#[derive(Debug, Default)]
pub struct Trace {
    pub events: Vec<Event>,
    /// Timestamp of the `START` control record, if present.
    pub start_us: Option<u64>,
    /// Timestamp of the `END` control record, if present (absent when the
    /// shell was killed before the EXIT trap could run).
    pub end_us: Option<u64>,
    /// Script exit code from the `END` record.
    pub exit_code: Option<i32>,
    /// Script path from the `START` record.
    pub script: Option<String>,
    /// Lines that were neither events, control records nor continuations.
    pub skipped: usize,
}

/// Parse `$EPOCHREALTIME`-style `seconds.micros` into microseconds.
///
/// bash formats the radix character according to the *locale*, so both
/// `1752.000042` and `1752,000042` must parse. The fractional part is
/// normalized to exactly six digits.
pub fn parse_epochrealtime(s: &str) -> Option<u64> {
    let s = s.trim();
    let (secs, frac) = match s.find(['.', ',']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let secs: u64 = secs.parse().ok()?;
    let mut digits = [0u8; 6];
    for (i, c) in frac.bytes().enumerate().take(6) {
        if !c.is_ascii_digit() {
            return None;
        }
        digits[i] = c - b'0';
    }
    let micros: u64 = digits.iter().fold(0u64, |acc, d| acc * 10 + *d as u64);
    secs.checked_mul(1_000_000)?.checked_add(micros)
}

/// Parse a whole trace file.
pub fn parse(input: &str) -> Trace {
    let mut trace = Trace::default();
    // Raw events including bootstrap-internal ones; filtered at the end so
    // continuation lines still attach to the right event first.
    let mut raw: Vec<Event> = Vec::new();

    for line in input.lines() {
        if let Some(rest) = line.strip_prefix(FIELD_SEP) {
            parse_control(rest, &mut trace);
            continue;
        }
        let plus = line.bytes().take_while(|&b| b == b'+').count();
        if plus > 0 && line[plus..].starts_with(FIELD_SEP) {
            match parse_event(plus as u32, &line[plus + FIELD_SEP.len_utf8()..]) {
                Some(ev) => raw.push(ev),
                None => trace.skipped += 1,
            }
            continue;
        }
        // Continuation of a multi-line command, or foreign noise before the
        // first event (which we count as skipped).
        match raw.last_mut() {
            Some(prev) => {
                prev.command.push('\n');
                prev.command.push_str(line);
            }
            None => trace.skipped += 1,
        }
    }

    raw.retain(|ev| !is_internal(ev));
    trace.events = raw;
    trace
}

/// Bootstrap-internal events that must not show up in a profile: the trap
/// dispatch, everything inside `__bashprof_finish`, and the `. "$0"` line
/// itself (its `BASH_SOURCE` is empty because it runs in `bash -c` context).
fn is_internal(ev: &Event) -> bool {
    ev.source.is_empty()
        || ev.func.starts_with(INTERNAL_PREFIX)
        || ev.command.starts_with(INTERNAL_PREFIX)
}

fn parse_control(rest: &str, trace: &mut Trace) {
    let mut fields = rest.split(FIELD_SEP);
    match (fields.next(), fields.next(), fields.next()) {
        (Some("START"), Some(ts), script) => {
            trace.start_us = parse_epochrealtime(ts);
            trace.script = script.map(str::to_owned).filter(|s| !s.is_empty());
        }
        (Some("END"), Some(ts), rc) => {
            trace.end_us = parse_epochrealtime(ts);
            trace.exit_code = rc.and_then(|r| r.trim().parse().ok());
        }
        _ => trace.skipped += 1,
    }
}

fn parse_event(subshell: u32, rest: &str) -> Option<Event> {
    // ts US source US lineno US func US funcdepth US command
    let mut it = rest.splitn(6, FIELD_SEP);
    let ts_us = parse_epochrealtime(it.next()?)?;
    let source = it.next()?.to_owned();
    let line: u32 = it.next()?.parse().ok()?;
    let func = it.next()?.to_owned();
    // Unset FUNCNAME (top-level code) expands to an empty field: depth 0.
    let depth_field = it.next()?;
    let func_depth: u32 = if depth_field.is_empty() {
        0
    } else {
        depth_field.parse().ok()?
    };
    let command = it.next()?.to_owned();
    Some(Event {
        subshell,
        ts_us,
        source,
        line,
        func,
        func_depth,
        command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: char = FIELD_SEP;

    /// Build one well-formed trace line the way bash + our PS4 would.
    fn line(plus: usize, ts: &str, src: &str, ln: u32, func: &str, fd: u32, cmd: &str) -> String {
        format!(
            "{}{US}{ts}{US}{src}{US}{ln}{US}{func}{US}{fd}{US}{cmd}",
            "+".repeat(plus)
        )
    }

    #[test]
    fn epochrealtime_parses_both_locale_radixes_and_normalizes_fractions() {
        // bash formats the radix per locale: de_DE emits a comma.
        assert_eq!(parse_epochrealtime("1752.000042"), Some(1_752_000_042));
        assert_eq!(parse_epochrealtime("1752,000042"), Some(1_752_000_042));
        // "1.5" is 1 second + 500000 us, not 1 second + 5 us.
        assert_eq!(parse_epochrealtime("1.5"), Some(1_500_000));
        // Beyond microsecond precision: truncate, never round up.
        assert_eq!(parse_epochrealtime("1.1234567"), Some(1_123_456));
    }

    #[test]
    fn epochrealtime_rejects_garbage() {
        assert_eq!(parse_epochrealtime("abc"), None);
        assert_eq!(parse_epochrealtime("1.2x"), None);
        assert_eq!(parse_epochrealtime(""), None);
    }

    #[test]
    fn parses_a_minimal_event() {
        let t = parse(&line(1, "10.000001", "./x.sh", 3, "", 1, "echo hi"));
        assert_eq!(t.events.len(), 1);
        let ev = &t.events[0];
        assert_eq!(ev.subshell, 1);
        assert_eq!(ev.ts_us, 10_000_001);
        assert_eq!(ev.source, "./x.sh");
        assert_eq!(ev.line, 3);
        assert_eq!(ev.func, "");
        assert_eq!(ev.func_depth, 1);
        assert_eq!(ev.command, "echo hi");
    }

    #[test]
    fn empty_depth_field_means_top_level_code() {
        // At the top level FUNCNAME is unset and the guarded PS4 expansion
        // yields an empty field; that must parse as depth 0, not an error.
        let input = format!("+{US}10.0{US}x.sh{US}1{US}{US}{US}echo hi");
        let t = parse(&input);
        assert_eq!(t.events.len(), 1);
        assert_eq!(t.events[0].func_depth, 0);
        assert_eq!(t.events[0].func, "");
    }

    #[test]
    fn leading_plus_count_is_the_subshell_depth() {
        let t = parse(&line(3, "10.0", "x.sh", 1, "", 1, "true"));
        assert_eq!(t.events[0].subshell, 3);
    }

    #[test]
    fn command_may_contain_the_field_separator() {
        // splitn keeps everything after the fifth separator as the command.
        let cmd = format!("printf '{US}weird{US}'");
        let t = parse(&line(1, "10.0", "x.sh", 1, "", 1, &cmd));
        assert_eq!(t.events[0].command, cmd);
    }

    #[test]
    fn continuation_lines_attach_to_the_previous_command() {
        let input = format!(
            "{}\nsecond half'\n{}",
            line(1, "10.0", "x.sh", 1, "", 1, "echo $'first"),
            line(1, "11.0", "x.sh", 2, "", 1, "true")
        );
        let t = parse(&input);
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.events[0].command, "echo $'first\nsecond half'");
        assert_eq!(t.skipped, 0);
    }

    #[test]
    fn start_and_end_control_records_are_extracted() {
        let input = format!(
            "{US}START{US}10.0{US}./job.sh\n{}\n{US}END{US}12.5{US}3",
            line(1, "10.5", "./job.sh", 1, "", 1, "true")
        );
        let t = parse(&input);
        assert_eq!(t.start_us, Some(10_000_000));
        assert_eq!(t.end_us, Some(12_500_000));
        assert_eq!(t.exit_code, Some(3));
        assert_eq!(t.script.as_deref(), Some("./job.sh"));

        // A kill -9 means the EXIT trap never ran: END is simply absent.
        let t = parse(&line(1, "10.0", "x.sh", 1, "", 1, "true"));
        assert_eq!(t.end_us, None);
        assert_eq!(t.exit_code, None);
    }

    #[test]
    fn bootstrap_internal_events_are_filtered() {
        let input = [
            line(1, "10.0", "x.sh", 1, "", 1, "real"),
            // The trap dispatch line: command is the internal function name.
            line(1, "11.0", "x.sh", 9, "", 1, "__bashprof_finish"),
            // Inside the internal function.
            line(1, "11.1", "x.sh", 9, "__bashprof_finish", 2, "set +x"),
            // The `. "$0"` line runs in `bash -c` context: empty source.
            line(1, "9.9", "", 0, "", 0, ". ./x.sh"),
        ]
        .join("\n");
        let t = parse(&input);
        assert_eq!(t.events.len(), 1);
        assert_eq!(t.events[0].command, "real");
    }

    #[test]
    fn garbled_and_foreign_lines_are_counted_not_fatal() {
        // A bad timestamp, plus stderr-style noise before the first event:
        // both are skipped, neither kills the parse.
        let input = format!(
            "random noise first\n{}\n+{US}notatime{US}x.sh{US}1{US}{US}1{US}cmd\n{}",
            line(1, "10.0", "x.sh", 1, "", 1, "a"),
            line(1, "11.0", "x.sh", 2, "", 1, "b")
        );
        let t = parse(&input);
        assert_eq!(t.events.len(), 2);
        assert_eq!(t.events[0].command, "a");
        assert_eq!(t.skipped, 2);
    }

    #[test]
    fn empty_input_yields_an_empty_trace() {
        let t = parse("");
        assert!(t.events.is_empty());
        assert_eq!(t.skipped, 0);
    }
}
