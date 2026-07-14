//! Command-line surface: parsing (pure, unit-tested) and dispatch.
//!
//! Exit codes: `2` for usage errors, `1` for runtime failures (unreadable
//! trace, missing file, shell too old), and for `bashprof run` the profiled
//! script's own exit code is passed through — so wrapping a CI step in
//! bashprof never hides its failure.

use crate::report::{Options, SortKey};
use crate::{collapse, jsonout, profile, report, runner, trace};
use std::path::PathBuf;

pub const USAGE: &str = "\
bashprof — line-level time profiler for bash scripts

USAGE:
    bashprof <COMMAND> [OPTIONS]

COMMANDS:
    run        Profile a script:  bashprof run [OPTIONS] [--] <script> [args...]
    report     Analyze a saved trace:  bashprof report [OPTIONS] <trace>
    collapse   Flamegraph-ready collapsed stacks from a trace
    annotate   Print script source annotated with per-line time

OPTIONS (run):
    --shell <PATH>   bash binary to profile with (default: bash from PATH)
    --out <FILE>     keep the raw trace at FILE for later report/collapse/annotate

OPTIONS (run, report):
    --top <N>        rows in the hot-line table (default 15, 0 = all)
    --sort <KEY>     self | count | line (default: self)
    --min-us <N>     hide lines with less than N microseconds of self-time
    --json           machine-readable JSON instead of the table

OPTIONS (annotate):
    --script <FILE>  source file to annotate (default: the traced script)

GLOBAL:
    -h, --help       show this help
    -V, --version    show version

`bashprof run` exits with the profiled script's exit code.
";

/// A fully parsed invocation.
#[derive(Debug, PartialEq)]
pub enum Action {
    Help,
    Version,
    Run {
        shell: String,
        out: Option<PathBuf>,
        opts: Options,
        json: bool,
        script: PathBuf,
        args: Vec<String>,
    },
    Report {
        trace: PathBuf,
        opts: Options,
        json: bool,
    },
    Collapse {
        trace: PathBuf,
    },
    Annotate {
        trace: PathBuf,
        script: Option<PathBuf>,
    },
}

/// Parse an argument vector (without the program name).
pub fn parse(args: &[String]) -> Result<Action, String> {
    let mut it = args.iter().peekable();
    let cmd = match it.next() {
        None => return Ok(Action::Help),
        Some(c) => c.as_str(),
    };
    match cmd {
        "-h" | "--help" | "help" => Ok(Action::Help),
        "-V" | "--version" | "version" => Ok(Action::Version),
        "run" => parse_run(&mut it),
        "report" => parse_report(&mut it),
        "collapse" => {
            let trace = one_positional(&mut it, "collapse", "<trace>")?;
            Ok(Action::Collapse { trace })
        }
        "annotate" => parse_annotate(&mut it),
        other => Err(format!("unknown command '{other}' (see bashprof --help)")),
    }
}

type ArgIter<'a> = std::iter::Peekable<std::slice::Iter<'a, String>>;

fn value(it: &mut ArgIter, flag: &str) -> Result<String, String> {
    it.next()
        .map(|s| s.to_owned())
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_usize(s: &str, flag: &str) -> Result<usize, String> {
    s.parse()
        .map_err(|_| format!("{flag}: '{s}' is not a number"))
}

fn parse_u64(s: &str, flag: &str) -> Result<u64, String> {
    s.parse()
        .map_err(|_| format!("{flag}: '{s}' is not a number"))
}

fn parse_shared_opt(
    arg: &str,
    it: &mut ArgIter,
    opts: &mut Options,
    json: &mut bool,
) -> Result<bool, String> {
    match arg {
        "--top" => {
            opts.top = parse_usize(&value(it, "--top")?, "--top")?;
            Ok(true)
        }
        "--sort" => {
            let v = value(it, "--sort")?;
            opts.sort = SortKey::parse(&v)
                .ok_or_else(|| format!("--sort: '{v}' is not one of self|count|line"))?;
            Ok(true)
        }
        "--min-us" => {
            opts.min_us = parse_u64(&value(it, "--min-us")?, "--min-us")?;
            Ok(true)
        }
        "--json" => {
            *json = true;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_run(it: &mut ArgIter) -> Result<Action, String> {
    let mut shell = "bash".to_owned();
    let mut out = None;
    let mut opts = Options::default();
    let mut json = false;
    let mut script: Option<PathBuf> = None;
    let mut args: Vec<String> = Vec::new();

    while let Some(arg) = it.next() {
        if script.is_some() {
            // Everything after the script belongs to the script.
            args.push(arg.clone());
            continue;
        }
        if arg == "--" {
            script = it
                .next()
                .map(PathBuf::from)
                .ok_or("run: missing <script> after --")?
                .into();
            continue;
        }
        if parse_shared_opt(arg, it, &mut opts, &mut json)? {
            continue;
        }
        match arg.as_str() {
            "--shell" => shell = value(it, "--shell")?,
            "--out" => out = Some(PathBuf::from(value(it, "--out")?)),
            a if a.starts_with('-') => {
                return Err(format!("run: unknown option '{a}'"));
            }
            a => script = Some(PathBuf::from(a)),
        }
    }
    let script = script
        .ok_or("run: missing <script> (usage: bashprof run [OPTIONS] [--] <script> [args...])")?;
    Ok(Action::Run {
        shell,
        out,
        opts,
        json,
        script,
        args,
    })
}

fn parse_report(it: &mut ArgIter) -> Result<Action, String> {
    let mut opts = Options::default();
    let mut json = false;
    let mut trace: Option<PathBuf> = None;
    while let Some(arg) = it.next() {
        if parse_shared_opt(arg, it, &mut opts, &mut json)? {
            continue;
        }
        match arg.as_str() {
            a if a.starts_with('-') => return Err(format!("report: unknown option '{a}'")),
            a => {
                if trace.replace(PathBuf::from(a)).is_some() {
                    return Err("report: takes exactly one <trace>".into());
                }
            }
        }
    }
    let trace = trace.ok_or("report: missing <trace>")?;
    Ok(Action::Report { trace, opts, json })
}

fn parse_annotate(it: &mut ArgIter) -> Result<Action, String> {
    let mut script: Option<PathBuf> = None;
    let mut trace: Option<PathBuf> = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--script" => script = Some(PathBuf::from(value(it, "--script")?)),
            a if a.starts_with('-') => return Err(format!("annotate: unknown option '{a}'")),
            a => {
                if trace.replace(PathBuf::from(a)).is_some() {
                    return Err("annotate: takes exactly one <trace>".into());
                }
            }
        }
    }
    let trace = trace.ok_or("annotate: missing <trace>")?;
    Ok(Action::Annotate { trace, script })
}

fn one_positional(it: &mut ArgIter, cmd: &str, name: &str) -> Result<PathBuf, String> {
    let mut pos: Option<PathBuf> = None;
    for arg in it {
        if arg.starts_with('-') {
            return Err(format!("{cmd}: unknown option '{arg}'"));
        }
        if pos.replace(PathBuf::from(arg)).is_some() {
            return Err(format!("{cmd}: takes exactly one {name}"));
        }
    }
    pos.ok_or_else(|| format!("{cmd}: missing {name}"))
}

/// Execute a parsed action. Returns the process exit code.
pub fn dispatch(action: Action) -> i32 {
    match action {
        Action::Help => {
            print!("{USAGE}");
            0
        }
        Action::Version => {
            println!("bashprof {}", crate::VERSION);
            0
        }
        Action::Run {
            shell,
            out,
            opts,
            json,
            script,
            args,
        } => cmd_run(shell, out, opts, json, script, args),
        Action::Report { trace, opts, json } => match load_profile(&trace) {
            Ok(p) => {
                emit_profile(&p, &opts, json);
                0
            }
            Err(e) => runtime_error(&e),
        },
        Action::Collapse { trace } => match load_profile(&trace) {
            Ok(p) => {
                print!("{}", collapse::render(&p));
                0
            }
            Err(e) => runtime_error(&e),
        },
        Action::Annotate { trace, script } => cmd_annotate(&trace, script.as_deref()),
    }
}

fn cmd_run(
    shell: String,
    out: Option<PathBuf>,
    opts: Options,
    json: bool,
    script: PathBuf,
    args: Vec<String>,
) -> i32 {
    if let Err(e) = runner::probe_shell(&shell) {
        return runtime_error(&e);
    }
    let (trace_path, keep) = match out {
        Some(p) => (p, true),
        None => (runner::temp_trace_path(), false),
    };
    // Start from an empty trace even if the file pre-exists.
    if std::fs::write(&trace_path, b"").is_err() {
        return runtime_error(&format!("cannot write trace file {}", trace_path.display()));
    }
    let spec = runner::RunSpec {
        shell,
        script,
        args,
        trace_path: trace_path.clone(),
    };
    let code = match runner::run(&spec) {
        Ok(code) => code,
        Err(e) => {
            if !keep {
                runner::remove_temp(&trace_path);
            }
            return runtime_error(&e);
        }
    };
    // A profiled script that installs its own EXIT trap replaces the
    // bootstrap's; backfill the END record so totals stay honest.
    runner::ensure_end_record(&trace_path, code);
    match load_profile(&trace_path) {
        Ok(p) => emit_profile(&p, &opts, json),
        Err(e) => {
            eprintln!("bashprof: {e}");
        }
    }
    if keep {
        eprintln!("bashprof: raw trace kept at {}", trace_path.display());
    } else {
        runner::remove_temp(&trace_path);
    }
    code
}

fn cmd_annotate(trace_path: &std::path::Path, script: Option<&std::path::Path>) -> i32 {
    let p = match load_profile(trace_path) {
        Ok(p) => p,
        Err(e) => return runtime_error(&e),
    };
    let target: PathBuf = match script {
        Some(s) => s.to_path_buf(),
        None => match &p.script {
            Some(s) => PathBuf::from(s),
            None => {
                return runtime_error("trace has no recorded script path; pass --script <FILE>")
            }
        },
    };
    let content = match std::fs::read_to_string(&target) {
        Ok(c) => c,
        Err(e) => return runtime_error(&format!("cannot read {}: {e}", target.display())),
    };
    print!(
        "{}",
        report::render_annotate(&p, &target.to_string_lossy(), &content)
    );
    0
}

fn load_profile(path: &std::path::Path) -> Result<profile::Profile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read trace {}: {e}", path.display()))?;
    let t = trace::parse(&raw);
    if t.events.is_empty() && t.start_us.is_none() {
        return Err(format!(
            "{} does not look like a bashprof trace (no events, no START record)",
            path.display()
        ));
    }
    Ok(profile::build(&t))
}

fn emit_profile(p: &profile::Profile, opts: &Options, json: bool) {
    if json {
        print!("{}", jsonout::render(p));
    } else {
        print!("{}", report::render(p, opts));
    }
}

fn runtime_error(msg: &str) -> i32 {
    eprintln!("bashprof: {msg}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Action, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn bare_invocation_and_flags_map_to_help_and_version() {
        assert_eq!(p(&[]), Ok(Action::Help));
        assert_eq!(p(&["--help"]), Ok(Action::Help));
        assert_eq!(p(&["-h"]), Ok(Action::Help));
        assert_eq!(p(&["--version"]), Ok(Action::Version));
        assert_eq!(p(&["-V"]), Ok(Action::Version));
    }

    #[test]
    fn unknown_command_is_a_usage_error() {
        let err = p(&["flame"]).unwrap_err();
        assert!(err.contains("unknown command 'flame'"));
    }

    #[test]
    fn run_parses_defaults() {
        match p(&["run", "job.sh"]).unwrap() {
            Action::Run {
                shell,
                out,
                opts,
                json,
                script,
                args,
            } => {
                assert_eq!(shell, "bash");
                assert_eq!(out, None);
                assert_eq!(opts.top, 15);
                assert_eq!(opts.sort, SortKey::SelfTime);
                assert!(!json);
                assert_eq!(script, PathBuf::from("job.sh"));
                assert!(args.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn run_everything_after_the_script_goes_to_the_script() {
        // `--json` here belongs to job.sh, not to bashprof.
        match p(&["run", "job.sh", "--json", "-x"]).unwrap() {
            Action::Run { json, args, .. } => {
                assert!(!json);
                assert_eq!(args, vec!["--json", "-x"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn run_double_dash_separates_script_from_options() {
        match p(&["run", "--top", "5", "--", "--weird-name.sh", "arg"]).unwrap() {
            Action::Run {
                opts, script, args, ..
            } => {
                assert_eq!(opts.top, 5);
                assert_eq!(script, PathBuf::from("--weird-name.sh"));
                assert_eq!(args, vec!["arg"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn run_accepts_shell_out_sort_min_us_json() {
        match p(&[
            "run",
            "--shell",
            "/opt/bash",
            "--out",
            "t.trace",
            "--sort",
            "count",
            "--min-us",
            "250",
            "--json",
            "job.sh",
        ])
        .unwrap()
        {
            Action::Run {
                shell,
                out,
                opts,
                json,
                ..
            } => {
                assert_eq!(shell, "/opt/bash");
                assert_eq!(out, Some(PathBuf::from("t.trace")));
                assert_eq!(opts.sort, SortKey::Count);
                assert_eq!(opts.min_us, 250);
                assert!(json);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn run_usage_errors_are_specific() {
        assert!(p(&["run"]).unwrap_err().contains("missing <script>"));
        assert!(p(&["run", "--frobnicate", "job.sh"])
            .unwrap_err()
            .contains("unknown option '--frobnicate'"));
        assert!(p(&["run", "--sort", "speed", "job.sh"])
            .unwrap_err()
            .contains("self|count|line"));
        assert!(p(&["run", "--top", "many", "job.sh"])
            .unwrap_err()
            .contains("not a number"));
        assert!(p(&["run", "--out"])
            .unwrap_err()
            .contains("--out requires a value"));
        assert!(p(&["run", "--"])
            .unwrap_err()
            .contains("missing <script> after --"));
    }

    #[test]
    fn report_parses_trace_options_and_arity() {
        match p(&["report", "--top", "0", "t.trace"]).unwrap() {
            Action::Report { trace, opts, json } => {
                assert_eq!(trace, PathBuf::from("t.trace"));
                assert_eq!(opts.top, 0);
                assert!(!json);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(p(&["report"]).unwrap_err().contains("missing <trace>"));
        assert!(p(&["report", "a", "b"])
            .unwrap_err()
            .contains("exactly one"));
    }

    #[test]
    fn collapse_parses_a_single_trace() {
        assert_eq!(
            p(&["collapse", "t.trace"]),
            Ok(Action::Collapse {
                trace: PathBuf::from("t.trace")
            })
        );
        assert!(p(&["collapse"]).unwrap_err().contains("missing <trace>"));
    }

    #[test]
    fn annotate_parses_trace_and_optional_script() {
        assert_eq!(
            p(&["annotate", "t.trace"]),
            Ok(Action::Annotate {
                trace: PathBuf::from("t.trace"),
                script: None
            })
        );
        assert_eq!(
            p(&["annotate", "--script", "lib.sh", "t.trace"]),
            Ok(Action::Annotate {
                trace: PathBuf::from("t.trace"),
                script: Some(PathBuf::from("lib.sh"))
            })
        );
    }

    #[test]
    fn usage_text_documents_every_command() {
        for cmd in ["run", "report", "collapse", "annotate"] {
            assert!(USAGE.contains(cmd), "USAGE must mention {cmd}");
        }
        assert!(USAGE.contains("exit code"));
    }
}
