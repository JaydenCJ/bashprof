//! Turn parsed events into a profile: per-line and per-function self-time,
//! call counts, and stack samples for the collapsed (flamegraph) output.
//!
//! ## Attribution model
//!
//! xtrace stamps the *start* of every simple command. The self-time of an
//! event is therefore the gap to the next event — which is exactly the time
//! bash spent executing that command (including any external process it
//! spawned) before reaching the next one. The final event is closed by the
//! `END` control record the bootstrap's EXIT trap writes.
//!
//! A line that *calls a function* is only charged for the dispatch itself;
//! the callee's lines carry their own time. The collapsed output preserves
//! the whole call stack, so flamegraph tooling renders inclusive time by
//! stacking frames — no double counting.
//!
//! ## Stack reconstruction
//!
//! Events carry `${FUNCNAME[0]}` (current function) and the frame count
//! rather than the joined stack (see `ps4`). bash keeps `FUNCNAME` unset at
//! the top level of a sourced script and includes the implicit `source`
//! frame once a function runs, so the raw counts arrive as 0 (top level),
//! 2 (first function level), 3, ... — the effective depth is
//! `max(count - 1, 0)`. The full stack is rebuilt by replaying depth
//! transitions: deeper by one pushes the current function, shallower
//! truncates, same-depth-different-name is a sibling call (pop + push). A
//! depth jump of more than one (a trap firing inside a deep callee, code
//! under a nested `source`) inserts `?` placeholder frames — visible,
//! honest, and rare.

use crate::trace::{Event, Trace};
use std::collections::HashMap;

/// Root frame name used for top-level script code.
pub const ROOT_FRAME: &str = "main";

/// Aggregated statistics for one `file:line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineStat {
    pub file: String,
    pub line: u32,
    /// How many times a command starting on this line was executed.
    pub count: u64,
    /// Total self-time in microseconds.
    pub self_us: u64,
    /// Largest single execution, to spot "usually fast, once slow" lines.
    pub max_us: u64,
    /// The command text most recently seen on this line.
    pub command: String,
}

/// Aggregated statistics for one function (leaf frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncStat {
    pub name: String,
    /// Number of times the function was entered.
    pub calls: u64,
    /// Commands executed with this function as the innermost frame.
    pub commands: u64,
    /// Self-time of those commands, microseconds.
    pub self_us: u64,
}

/// One attributed sample: a full stack, its source location, and self-time.
/// This is the unit the collapsed output is folded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Outermost-first, starting with [`ROOT_FRAME`].
    pub stack: Vec<String>,
    pub file: String,
    pub line: u32,
    pub self_us: u64,
}

/// The complete analysis result.
#[derive(Debug, Default)]
pub struct Profile {
    pub script: Option<String>,
    pub exit_code: Option<i32>,
    /// Wall time from START to END (falls back to first..last event).
    pub total_us: u64,
    /// Number of traced commands.
    pub commands: u64,
    /// Distinct source files seen, sorted.
    pub files: Vec<String>,
    /// Per-line stats, sorted by file then line.
    pub lines: Vec<LineStat>,
    /// Per-function stats, sorted by self-time descending.
    pub funcs: Vec<FuncStat>,
    /// One sample per event, in execution order.
    pub samples: Vec<Sample>,
    /// Garbled trace lines the parser had to skip.
    pub skipped: usize,
}

/// Build a [`Profile`] from a parsed [`Trace`].
pub fn build(trace: &Trace) -> Profile {
    let events = &trace.events;
    let mut profile = Profile {
        script: trace.script.clone(),
        exit_code: trace.exit_code,
        skipped: trace.skipped,
        commands: events.len() as u64,
        ..Profile::default()
    };
    if events.is_empty() {
        profile.total_us = wall_time(trace, 0, 0);
        return profile;
    }

    let first_ts = events.first().map(|e| e.ts_us).unwrap_or(0);
    let last_ts = events.last().map(|e| e.ts_us).unwrap_or(0);
    profile.total_us = wall_time(trace, first_ts, last_ts);

    let mut stack: Vec<String> = Vec::new();
    let mut line_stats: HashMap<(String, u32), LineStat> = HashMap::new();
    let mut func_stats: HashMap<String, FuncStat> = HashMap::new();

    for (i, ev) in events.iter().enumerate() {
        let next_ts = match events.get(i + 1) {
            Some(next) => next.ts_us,
            None => trace.end_us.unwrap_or(ev.ts_us),
        };
        // Background jobs and subshells can interleave writes; a negative
        // gap means overlap, which we clamp rather than corrupt totals with.
        let self_us = next_ts.saturating_sub(ev.ts_us);

        let entered = adjust_stack(&mut stack, ev);
        let leaf = stack.last().map(String::as_str).unwrap_or(ROOT_FRAME);

        let fs = func_stats
            .entry(leaf.to_owned())
            .or_insert_with(|| FuncStat {
                name: leaf.to_owned(),
                calls: 0,
                commands: 0,
                self_us: 0,
            });
        if entered {
            fs.calls += 1;
        }
        fs.commands += 1;
        fs.self_us += self_us;

        let key = (ev.source.clone(), ev.line);
        let ls = line_stats.entry(key).or_insert_with(|| LineStat {
            file: ev.source.clone(),
            line: ev.line,
            count: 0,
            self_us: 0,
            max_us: 0,
            command: String::new(),
        });
        ls.count += 1;
        ls.self_us += self_us;
        ls.max_us = ls.max_us.max(self_us);
        ls.command = ev.command.clone();

        let mut full = Vec::with_capacity(stack.len() + 1);
        full.push(ROOT_FRAME.to_owned());
        full.extend(stack.iter().cloned());
        profile.samples.push(Sample {
            stack: full,
            file: ev.source.clone(),
            line: ev.line,
            self_us,
        });
    }

    profile.lines = {
        let mut v: Vec<LineStat> = line_stats.into_values().collect();
        v.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
        v
    };
    profile.funcs = {
        let mut v: Vec<FuncStat> = func_stats.into_values().collect();
        v.sort_by(|a, b| b.self_us.cmp(&a.self_us).then(a.name.cmp(&b.name)));
        v
    };
    profile.files = {
        let mut v: Vec<String> = profile
            .lines
            .iter()
            .map(|l| l.file.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        v.sort();
        v
    };
    profile
}

fn wall_time(trace: &Trace, first_ts: u64, last_ts: u64) -> u64 {
    let start = trace.start_us.unwrap_or(first_ts);
    let end = trace.end_us.unwrap_or(last_ts);
    end.saturating_sub(start)
}

/// Replay one event's depth transition onto the reconstructed stack.
/// Returns `true` when this event *entered* a function (a new call).
fn adjust_stack(stack: &mut Vec<String>, ev: &Event) -> bool {
    // Raw counts: 0 = top level (FUNCNAME unset), 2 = first function level
    // (the implicit `source` frame is included). Effective depth is one
    // less, saturating so a raw 0 cannot underflow.
    let depth = ev.func_depth.saturating_sub(1) as usize;
    if depth == 0 {
        stack.clear();
        return false;
    }
    let mut entered = false;
    if depth > stack.len() {
        // Fill any gap with placeholders (trap fired straight into a deep
        // frame), then the known current function on top.
        while stack.len() < depth - 1 {
            stack.push("?".to_owned());
        }
        stack.push(ev.func.clone());
        entered = true;
    } else {
        stack.truncate(depth);
        if stack.last().map(String::as_str) != Some(ev.func.as_str()) {
            // Sibling call at the same depth: f returned, g was called.
            stack.pop();
            stack.push(ev.func.clone());
            entered = true;
        }
    }
    entered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::Event;

    /// Event factory. Real raw depths: 0 = top level (FUNCNAME unset),
    /// 2 = first function level (includes the implicit `source` frame).
    fn ev(ts_us: u64, line: u32, func: &str, func_depth: u32, cmd: &str) -> Event {
        Event {
            subshell: 1,
            ts_us,
            source: "job.sh".into(),
            line,
            func: func.into(),
            func_depth,
            command: cmd.into(),
        }
    }

    fn trace_of(events: Vec<Event>, end_us: Option<u64>) -> Trace {
        let start_us = events.first().map(|e| e.ts_us);
        Trace {
            events,
            start_us,
            end_us,
            exit_code: end_us.map(|_| 0),
            script: Some("job.sh".into()),
            skipped: 0,
        }
    }

    #[test]
    fn self_time_is_the_gap_to_the_next_event() {
        let t = trace_of(
            vec![ev(1_000, 1, "", 0, "a"), ev(4_000, 2, "", 0, "b")],
            Some(4_500),
        );
        let p = build(&t);
        assert_eq!(p.lines[0].self_us, 3_000); // line 1: 4000-1000
        assert_eq!(p.lines[1].self_us, 500); // line 2: closed by END

        // Killed shell: no END record, so the last line gets zero rather
        // than a fabricated duration.
        let t = trace_of(vec![ev(1_000, 1, "", 0, "a")], None);
        assert_eq!(build(&t).lines[0].self_us, 0);
    }

    #[test]
    fn negative_gaps_from_interleaved_subshells_are_clamped() {
        // A background job's event can land in the file *after* a later
        // foreground event; the delta must clamp to 0, not wrap around.
        let t = trace_of(
            vec![ev(5_000, 1, "", 0, "a"), ev(2_000, 2, "", 0, "bg")],
            Some(6_000),
        );
        let p = build(&t);
        assert_eq!(p.lines[0].self_us, 0);
        assert_eq!(p.lines[1].self_us, 4_000);
    }

    #[test]
    fn repeated_lines_accumulate_count_max_and_latest_command() {
        let t = trace_of(
            vec![
                ev(0, 7, "", 0, "step 1"),
                ev(100, 7, "", 0, "step 2"),
                ev(5_100, 7, "", 0, "step 3"),
            ],
            Some(5_200),
        );
        let p = build(&t);
        assert_eq!(p.lines.len(), 1);
        let l = &p.lines[0];
        assert_eq!(l.count, 3);
        assert_eq!(l.self_us, 100 + 5_000 + 100);
        // max_us spots "usually fast, once slow" lines.
        assert_eq!(l.max_us, 5_000);
        // Loop bodies re-expand variables; keep the latest expansion.
        assert_eq!(l.command, "step 3");
    }

    #[test]
    fn total_prefers_start_end_records_and_falls_back_to_event_span() {
        let mut t = trace_of(vec![ev(2_000, 1, "", 0, "a")], Some(9_000));
        t.start_us = Some(1_000);
        assert_eq!(build(&t).total_us, 8_000);

        let mut t = trace_of(
            vec![ev(1_000, 1, "", 0, "a"), ev(4_000, 2, "", 0, "b")],
            None,
        );
        t.start_us = None;
        assert_eq!(build(&t).total_us, 3_000);
    }

    #[test]
    fn top_level_code_is_attributed_to_main() {
        let t = trace_of(vec![ev(0, 1, "", 0, "a")], Some(10));
        let p = build(&t);
        assert_eq!(p.funcs.len(), 1);
        assert_eq!(p.funcs[0].name, ROOT_FRAME);
        assert_eq!(p.samples[0].stack, vec![ROOT_FRAME.to_owned()]);
    }

    #[test]
    fn function_entry_and_return_reconstruct_the_stack() {
        let t = trace_of(
            vec![
                ev(0, 10, "", 0, "work"),      // top level: calls work
                ev(10, 3, "work", 2, "step1"), // inside work
                ev(20, 4, "work", 2, "step2"),
                ev(30, 11, "", 0, "echo done"), // back at top level
            ],
            Some(40),
        );
        let p = build(&t);
        assert_eq!(p.samples[0].stack, vec!["main"]);
        assert_eq!(p.samples[1].stack, vec!["main", "work"]);
        assert_eq!(p.samples[2].stack, vec!["main", "work"]);
        assert_eq!(p.samples[3].stack, vec!["main"]);
        let work = p.funcs.iter().find(|f| f.name == "work").unwrap();
        assert_eq!(work.calls, 1);
        assert_eq!(work.commands, 2);
        assert_eq!(work.self_us, 10 + 10);
    }

    #[test]
    fn nested_calls_stack_frames_outermost_first() {
        let t = trace_of(
            vec![
                ev(0, 20, "", 0, "outer"),
                ev(1, 5, "outer", 2, "inner"),
                ev(2, 2, "inner", 3, "true"),
            ],
            Some(3),
        );
        let p = build(&t);
        assert_eq!(p.samples[2].stack, vec!["main", "outer", "inner"]);
    }

    #[test]
    fn sibling_call_at_same_depth_replaces_the_top_frame() {
        // f() { :; }; g() { :; }; f; g  — depth stays 2, name flips.
        let t = trace_of(
            vec![
                ev(0, 8, "", 0, "f"),
                ev(1, 1, "f", 2, ":"),
                ev(2, 2, "g", 2, ":"),
            ],
            Some(3),
        );
        let p = build(&t);
        assert_eq!(p.samples[1].stack, vec!["main", "f"]);
        assert_eq!(p.samples[2].stack, vec!["main", "g"]);
        let g = p.funcs.iter().find(|f| f.name == "g").unwrap();
        assert_eq!(g.calls, 1);
    }

    #[test]
    fn recursive_calls_count_each_entry() {
        // fact 3 -> fact 2 -> fact 1: three entries of the same name.
        let t = trace_of(
            vec![
                ev(0, 6, "", 0, "fact 3"),
                ev(1, 2, "fact", 2, "fact 2"),
                ev(2, 2, "fact", 3, "fact 1"),
                ev(3, 3, "fact", 4, "echo 1"),
            ],
            Some(4),
        );
        let p = build(&t);
        let f = p.funcs.iter().find(|f| f.name == "fact").unwrap();
        assert_eq!(f.calls, 3);
        assert_eq!(p.samples[3].stack, vec!["main", "fact", "fact", "fact"]);
    }

    #[test]
    fn depth_jump_inserts_placeholder_frames() {
        // First event at top level, then a jump straight to raw depth 4
        // (e.g. a trap firing inside a nested call chain we never saw
        // enter): unknown intermediate frames become "?".
        let t = trace_of(
            vec![ev(0, 1, "", 0, "a"), ev(1, 9, "deep", 4, "b")],
            Some(2),
        );
        let p = build(&t);
        assert_eq!(p.samples[1].stack, vec!["main", "?", "?", "deep"]);
    }

    #[test]
    fn subshell_depth_does_not_disturb_function_attribution() {
        // A command inside $( ) keeps the caller's FUNCNAME; only the
        // leading-plus count changes, which attribution ignores.
        let mut sub = ev(2, 4, "work", 2, "date");
        sub.subshell = 2;
        let t = trace_of(
            vec![ev(0, 9, "", 0, "work"), ev(1, 3, "work", 2, "x=1"), sub],
            Some(3),
        );
        let p = build(&t);
        assert_eq!(p.samples[2].stack, vec!["main", "work"]);
    }

    #[test]
    fn files_deduplicated_and_lines_sorted_by_file_then_number() {
        let mut lib = ev(1, 2, "", 0, "b");
        lib.source = "lib/util.sh".into();
        let t = trace_of(
            vec![
                ev(0, 9, "", 0, "z"),
                lib,
                ev(2, 2, "", 0, "a"),
                ev(3, 5, "", 0, "m"),
            ],
            Some(4),
        );
        let p = build(&t);
        assert_eq!(p.files, vec!["job.sh", "lib/util.sh"]);
        let keys: Vec<(&str, u32)> = p.lines.iter().map(|l| (l.file.as_str(), l.line)).collect();
        assert_eq!(
            keys,
            vec![
                ("job.sh", 2),
                ("job.sh", 5),
                ("job.sh", 9),
                ("lib/util.sh", 2)
            ]
        );
    }

    #[test]
    fn funcs_are_sorted_by_self_time_descending() {
        let t = trace_of(
            vec![
                ev(0, 9, "", 0, "fast"),  // 1us of top-level dispatch
                ev(1, 1, "fast", 2, "a"), // 1us
                ev(2, 2, "slow", 2, "b"), // 98us
            ],
            Some(100),
        );
        let p = build(&t);
        assert_eq!(p.funcs[0].name, "slow");
        assert_eq!(p.funcs[1].name, "fast");
        assert_eq!(p.funcs[2].name, ROOT_FRAME);
    }

    #[test]
    fn empty_trace_builds_an_empty_profile() {
        let t = Trace::default();
        let p = build(&t);
        assert_eq!(p.commands, 0);
        assert!(p.lines.is_empty());
        assert!(p.funcs.is_empty());
        assert_eq!(p.total_us, 0);
    }
}
