//! Human-facing rendering: the hot-line report and annotated source.

use crate::profile::{LineStat, Profile};

/// How the hot-line table is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Self-time descending (default — "where did the time go?").
    SelfTime,
    /// Execution count descending ("what ran the most?").
    Count,
    /// File and line number ascending (source order).
    Line,
}

impl SortKey {
    pub fn parse(s: &str) -> Option<SortKey> {
        match s {
            "self" => Some(SortKey::SelfTime),
            "count" => Some(SortKey::Count),
            "line" => Some(SortKey::Line),
            _ => None,
        }
    }
}

/// Report shaping options shared by `run` and `report`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Rows in the hot-line table; 0 means all.
    pub top: usize,
    pub sort: SortKey,
    /// Hide lines with less self-time than this (microseconds).
    pub min_us: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            top: 15,
            sort: SortKey::SelfTime,
            min_us: 0,
        }
    }
}

/// Format integer microseconds for humans: `842us`, `12.3ms`, `1.204s`,
/// `2m06s`. Non-breaking widths are not attempted; the table pads instead.
pub fn fmt_duration(us: u64) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 999_500 {
        // Above 999_499 the millisecond rendering would round to "1000ms";
        // hand that range to the seconds branch instead.
        let ms = us as f64 / 1_000.0;
        if ms < 100.0 {
            format!("{ms:.1}ms")
        } else {
            format!("{ms:.0}ms")
        }
    } else if us < 60_000_000 {
        format!("{:.3}s", us as f64 / 1_000_000.0)
    } else {
        let total_secs = us / 1_000_000;
        format!("{}m{:02}s", total_secs / 60, total_secs % 60)
    }
}

/// `1 command` / `3 commands` — count-prefixed noun with a plain `s` plural.
fn plural(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Percentage of `part` in `whole`, one decimal, safe for `whole == 0`.
pub fn fmt_percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "0.0%".to_owned();
    }
    format!("{:.1}%", part as f64 * 100.0 / whole as f64)
}

/// Order + filter the profile's lines according to `opts`.
pub fn select_lines<'a>(profile: &'a Profile, opts: &Options) -> Vec<&'a LineStat> {
    let mut lines: Vec<&LineStat> = profile
        .lines
        .iter()
        .filter(|l| l.self_us >= opts.min_us)
        .collect();
    match opts.sort {
        SortKey::SelfTime => lines.sort_by(|a, b| {
            b.self_us
                .cmp(&a.self_us)
                .then(a.file.cmp(&b.file))
                .then(a.line.cmp(&b.line))
        }),
        SortKey::Count => lines.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(a.file.cmp(&b.file))
                .then(a.line.cmp(&b.line))
        }),
        SortKey::Line => lines.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line))),
    }
    if opts.top > 0 && lines.len() > opts.top {
        lines.truncate(opts.top);
    }
    lines
}

/// Render the standard report.
pub fn render(profile: &Profile, opts: &Options) -> String {
    let mut out = String::new();
    let script = profile.script.as_deref().unwrap_or("<unknown script>");
    let exit = profile
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".to_owned());
    out.push_str(&format!(
        "bashprof {}: {script} (exit {exit})\n",
        crate::VERSION
    ));
    out.push_str(&format!(
        "total {} wall, {} traced, {}\n",
        fmt_duration(profile.total_us),
        plural(profile.commands, "command"),
        plural(profile.files.len() as u64, "source file")
    ));
    if profile.skipped > 0 {
        out.push_str(&format!(
            "note: {} skipped\n",
            plural(profile.skipped as u64, "unparseable trace line")
        ));
    }
    out.push('\n');

    let lines = select_lines(profile, opts);
    let loc_width = lines
        .iter()
        .map(|l| location(l).len())
        .chain(std::iter::once("LINE".len()))
        .max()
        .unwrap_or(4);

    out.push_str(&format!(
        "{:>9}  {:>6}  {:>6}  {:<loc_width$}  {}\n",
        "SELF", "%", "COUNT", "LINE", "COMMAND"
    ));
    for l in &lines {
        out.push_str(&format!(
            "{:>9}  {:>6}  {:>6}  {:<loc_width$}  {}\n",
            fmt_duration(l.self_us),
            fmt_percent(l.self_us, profile.total_us),
            l.count,
            location(l),
            first_line(&l.command)
        ));
    }
    let hidden = profile.lines.len().saturating_sub(lines.len());
    if hidden > 0 {
        // `hidden` counts both --top truncation and --min-us filtering, so
        // the hint must undo whichever filters are actually active.
        let hint = if opts.min_us > 0 {
            "--top 0 --min-us 0 shows all"
        } else {
            "--top 0 shows all"
        };
        out.push_str(&format!(
            "... {}; {hint}\n",
            plural(hidden as u64, "more line")
        ));
    }

    if profile.funcs.len() > 1 {
        out.push('\n');
        out.push_str("FUNCTIONS (self-time)\n");
        for f in &profile.funcs {
            // "main" is the top-level pseudo-frame; a call count would be
            // meaningless for it.
            let calls = if f.name == crate::profile::ROOT_FRAME {
                "-".to_owned()
            } else {
                format!("{}x", f.calls)
            };
            out.push_str(&format!(
                "{:>9}  {:>6}  {:>7}  {}\n",
                fmt_duration(f.self_us),
                fmt_percent(f.self_us, profile.total_us),
                calls,
                f.name
            ));
        }
    }
    out
}

/// Render a source file with per-line timing in the left gutter.
///
/// `source_name` is the key the trace recorded (what `BASH_SOURCE` said);
/// `content` is the file's text. Lines that never executed show a bare
/// gutter — that alone is a useful coverage view.
pub fn render_annotate(profile: &Profile, source_name: &str, content: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{:>9} {:>6}  {}\n", "SELF", "COUNT", source_name));
    for (idx, text) in content.lines().enumerate() {
        let lineno = (idx + 1) as u32;
        let stat = profile
            .lines
            .iter()
            .find(|l| l.line == lineno && source_matches(&l.file, source_name));
        match stat {
            Some(l) => out.push_str(&format!(
                "{:>9} {:>6}  {}\n",
                fmt_duration(l.self_us),
                l.count,
                text
            )),
            None => out.push_str(&format!("{:>9} {:>6}  {}\n", "-", "-", text)),
        }
    }
    out
}

/// Does a trace source key refer to `wanted`? Exact match first; otherwise
/// compare basenames, because the trace may say `./job.sh` while the user
/// passes `job.sh` (or an absolute path).
pub fn source_matches(trace_source: &str, wanted: &str) -> bool {
    if trace_source == wanted {
        return true;
    }
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_owned();
    base(trace_source) == base(wanted)
}

fn location(l: &LineStat) -> String {
    let file = l.file.rsplit('/').next().unwrap_or(&l.file);
    format!("{file}:{}", l.line)
}

/// Multi-line commands would break the table; show the first line + marker.
fn first_line(cmd: &str) -> String {
    match cmd.split_once('\n') {
        Some((first, _)) => format!("{first} ..."),
        None => cmd.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{FuncStat, LineStat};

    fn line(file: &str, n: u32, count: u64, self_us: u64, cmd: &str) -> LineStat {
        LineStat {
            file: file.into(),
            line: n,
            count,
            self_us,
            max_us: self_us,
            command: cmd.into(),
        }
    }

    fn profile() -> Profile {
        Profile {
            script: Some("./job.sh".into()),
            exit_code: Some(0),
            total_us: 2_000_000,
            commands: 6,
            files: vec!["job.sh".into()],
            lines: vec![
                line("job.sh", 3, 1, 1_500_000, "sleep 1.5"),
                line("job.sh", 5, 50, 400_000, "echo tick"),
                line("job.sh", 9, 1, 100_000, "date"),
            ],
            funcs: vec![
                FuncStat {
                    name: "main".into(),
                    calls: 0,
                    commands: 4,
                    self_us: 1_600_000,
                },
                FuncStat {
                    name: "work".into(),
                    calls: 2,
                    commands: 2,
                    self_us: 400_000,
                },
            ],
            samples: vec![],
            skipped: 0,
        }
    }

    #[test]
    fn duration_formatting_across_magnitudes() {
        assert_eq!(fmt_duration(0), "0us");
        assert_eq!(fmt_duration(842), "842us");
        assert_eq!(fmt_duration(12_300), "12.3ms"); // one decimal below 100ms
        assert_eq!(fmt_duration(999_499), "999ms");
        // Would render "1000ms" in the millisecond branch; must not.
        assert_eq!(fmt_duration(999_500), "1.000s");
        assert_eq!(fmt_duration(1_204_000), "1.204s");
        assert_eq!(fmt_duration(59_999_000), "59.999s");
        assert_eq!(fmt_duration(126_000_000), "2m06s");
    }

    #[test]
    fn percent_is_safe_for_zero_total() {
        assert_eq!(fmt_percent(5, 0), "0.0%");
        assert_eq!(fmt_percent(1, 2), "50.0%");
    }

    #[test]
    fn sort_key_parses_known_values_only() {
        assert_eq!(SortKey::parse("self"), Some(SortKey::SelfTime));
        assert_eq!(SortKey::parse("count"), Some(SortKey::Count));
        assert_eq!(SortKey::parse("line"), Some(SortKey::Line));
        assert_eq!(SortKey::parse("time"), None);
    }

    #[test]
    fn sort_modes_order_slowest_hottest_or_source_order() {
        let p = profile();
        let rows = select_lines(&p, &Options::default());
        assert_eq!(rows[0].line, 3, "default: slowest first");

        let by_count = Options {
            sort: SortKey::Count,
            ..Options::default()
        };
        assert_eq!(select_lines(&p, &by_count)[0].line, 5, "hottest loop first");

        let by_line = Options {
            sort: SortKey::Line,
            ..Options::default()
        };
        let nums: Vec<u32> = select_lines(&p, &by_line).iter().map(|l| l.line).collect();
        assert_eq!(nums, vec![3, 5, 9], "source order");
    }

    #[test]
    fn top_truncates_and_zero_means_all() {
        let p = profile();
        let top1 = Options {
            top: 1,
            ..Options::default()
        };
        assert_eq!(select_lines(&p, &top1).len(), 1);
        let all = Options {
            top: 0,
            ..Options::default()
        };
        assert_eq!(select_lines(&p, &all).len(), 3);
    }

    #[test]
    fn min_us_filters_fast_lines() {
        let p = profile();
        let opts = Options {
            min_us: 200_000,
            ..Options::default()
        };
        let rows = select_lines(&p, &opts);
        assert!(rows.iter().all(|l| l.self_us >= 200_000));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn ties_break_deterministically_by_file_and_line() {
        let mut p = profile();
        p.lines = vec![
            line("b.sh", 2, 1, 100, "x"),
            line("a.sh", 9, 1, 100, "y"),
            line("a.sh", 1, 1, 100, "z"),
        ];
        let rows = select_lines(&p, &Options::default());
        let keys: Vec<(String, u32)> = rows.iter().map(|l| (l.file.clone(), l.line)).collect();
        assert_eq!(
            keys,
            vec![("a.sh".into(), 1), ("a.sh".into(), 9), ("b.sh".into(), 2)]
        );
    }

    #[test]
    fn report_shows_header_rows_and_truncation_notice() {
        let out = render(&profile(), &Options::default());
        assert!(out.contains("./job.sh (exit 0)"));
        // Counts are pluralized properly: "1 source file", never "1 files".
        assert!(out.contains("total 2.000s wall, 6 commands traced, 1 source file"));
        assert!(!out.contains("file(s)"));
        assert!(out.contains("1.500s"));
        assert!(out.contains("75.0%"));
        assert!(out.contains("job.sh:3"));
        assert!(out.contains("sleep 1.5"));

        let truncated = render(
            &profile(),
            &Options {
                top: 1,
                ..Options::default()
            },
        );
        assert!(truncated.contains("... 2 more lines; --top 0 shows all"));
        let truncated_to_one = render(
            &profile(),
            &Options {
                top: 2,
                ..Options::default()
            },
        );
        assert!(truncated_to_one.contains("... 1 more line; --top 0 shows all"));
        // With --min-us active, "--top 0" alone would not show all: the
        // hint must name both filters.
        let filtered = render(
            &profile(),
            &Options {
                min_us: 200_000,
                ..Options::default()
            },
        );
        assert!(filtered.contains("... 1 more line; --top 0 --min-us 0 shows all"));
    }

    #[test]
    fn report_lists_functions_with_calls_but_omits_section_for_flat_scripts() {
        let out = render(&profile(), &Options::default());
        assert!(out.contains("FUNCTIONS (self-time)"));
        assert!(out.contains("2x  work"));
        // "main" is a pseudo-frame: no call count.
        assert!(out.contains("-  main"));

        let mut flat = profile();
        flat.funcs.truncate(1); // only "main"
        assert!(!render(&flat, &Options::default()).contains("FUNCTIONS"));
    }

    #[test]
    fn report_notes_skipped_lines_only_when_present() {
        let mut p = profile();
        assert!(!render(&p, &Options::default()).contains("skipped"));
        p.skipped = 2;
        assert!(render(&p, &Options::default()).contains("2 unparseable trace lines skipped"));
        p.skipped = 1;
        assert!(render(&p, &Options::default()).contains("1 unparseable trace line skipped"));
    }

    #[test]
    fn multiline_commands_are_flattened_in_the_table() {
        let mut p = profile();
        p.lines[0].command = "echo $'a\nb'".into();
        let out = render(&p, &Options::default());
        assert!(out.contains("echo $'a ..."));
        assert!(!out.contains("echo $'a\nb'"));
    }

    #[test]
    fn annotate_marks_executed_and_untouched_lines() {
        let p = profile();
        let src = "#!/usr/bin/env bash\n# comment\nsleep 1.5\n";
        let out = render_annotate(&p, "job.sh", src);
        let rows: Vec<&str> = out.lines().collect();
        assert!(rows[1].trim_start().starts_with("- ")); // shebang: never traced
        assert!(rows[3].contains("1.500s"));
        assert!(rows[3].contains("sleep 1.5"));
    }

    #[test]
    fn source_matching_is_exact_or_by_basename() {
        assert!(source_matches("./job.sh", "job.sh"));
        assert!(source_matches("/ci/job.sh", "job.sh"));
        assert!(source_matches("job.sh", "job.sh"));
        assert!(!source_matches("other.sh", "job.sh"));
        // Trace said "job.sh"; the user annotates "./examples/job.sh".
        let p = profile();
        let out = render_annotate(&p, "./examples/job.sh", "a\nb\nsleep 1.5\n");
        assert!(out.contains("1.500s"));
    }
}
