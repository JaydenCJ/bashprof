//! End-to-end tests against the compiled `bashprof` binary: live profiling
//! of real bash scripts (asserting on deterministic structure — counts,
//! line numbers, exit codes — never on wall-clock durations) and offline
//! analysis of synthetic traces with fixed timestamps, where the numbers
//! are asserted exactly. Everything runs in temporary directories, offline.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const US: char = '\u{1f}';

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_bashprof")
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("failed to run bashprof binary")
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bashprof-cli-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One synthetic xtrace line, exactly as bash + the bashprof PS4 emit it.
fn trace_line(
    plus: usize,
    ts: &str,
    src: &str,
    line: u32,
    func: &str,
    depth: &str,
    cmd: &str,
) -> String {
    format!(
        "{}{US}{ts}{US}{src}{US}{line}{US}{func}{US}{depth}{US}{cmd}",
        "+".repeat(plus)
    )
}

/// A synthetic trace with fixed timestamps: 2.0 seconds total, one slow
/// top-level line, a function called once with two inner commands.
fn fixture_trace() -> String {
    [
        format!("{US}START{US}100.000000{US}./job.sh"),
        trace_line(1, "100.100000", "./job.sh", 2, "", "", "setup"),
        trace_line(1, "100.200000", "./job.sh", 5, "", "", "work"),
        trace_line(1, "100.300000", "./job.sh", 10, "work", "2", "step_a"),
        trace_line(1, "100.400000", "./job.sh", 11, "work", "2", "step_b"),
        trace_line(1, "101.900000", "./job.sh", 6, "", "", "echo done"),
        format!("{US}END{US}102.000000{US}0"),
    ]
    .join("\n")
}

#[test]
fn help_lists_commands_and_version_matches_manifest() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    let text = stdout(&help);
    for cmd in ["run", "report", "collapse", "annotate"] {
        assert!(text.contains(cmd), "help must mention '{cmd}'");
    }

    let version = run(&["--version"]);
    assert!(version.status.success());
    assert_eq!(
        stdout(&version).trim(),
        format!("bashprof {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_exit_2_runtime_errors_exit_1() {
    let out = run(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("unknown command"));

    let out = run(&["run", "--sort", "vibes", "x.sh"]);
    assert_eq!(out.status.code(), Some(2));

    let out = run(&["run", "/nonexistent/missing.sh"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("script not found"));

    let out = run(&["report", "/nonexistent/missing.trace"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn run_profiles_a_real_script_with_correct_counts() {
    let dir = tempdir("run-basic");
    let script = dir.join("job.sh");
    fs::write(
        &script,
        "#!/usr/bin/env bash\n\
         work() {\n  x=$((1 + 1))\n}\n\
         for i in 1 2 3; do\n  work\ndone\n\
         echo finished\n",
    )
    .unwrap();
    let out = run(&["run", "--top", "0", script.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    // The script's own stdout is passed through, before the report.
    assert!(text.contains("finished"));
    assert!(text.contains("(exit 0)"));
    // Deterministic structure: the loop ran exactly 3 times.
    assert!(text.contains("3  job.sh:6  work"), "report was:\n{text}");
    assert!(text.contains("job.sh:3"), "function body line traced");
    assert!(text.contains("FUNCTIONS (self-time)"));
    assert!(text.contains("3x  work"), "work must show 3 calls:\n{text}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_passes_arguments_and_dollar_zero_through() {
    let dir = tempdir("run-args");
    let script = dir.join("args.sh");
    fs::write(
        &script,
        "echo \"argv0=$(basename \"$0\") one=$1 two=$2 n=$#\"\n",
    )
    .unwrap();
    let out = run(&["run", script.to_str().unwrap(), "alpha", "beta gamma"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("argv0=args.sh one=alpha two=beta gamma n=2"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_passes_the_script_exit_code_through() {
    let dir = tempdir("run-exit");
    let script = dir.join("fail.sh");
    fs::write(&script, "echo about to fail\nexit 7\n").unwrap();
    let out = run(&["run", script.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(7), "CI wrappers rely on this");
    // The report is still produced for a failing script.
    assert!(stdout(&out).contains("(exit 7)"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_survives_strict_mode_scripts() {
    // set -euo pipefail + IFS=$'\n\t' is the classic "unofficial strict
    // mode"; an unguarded FUNCNAME expansion in PS4 or an IFS-joined stack
    // field would break exactly here.
    let dir = tempdir("run-strict");
    let script = dir.join("strict.sh");
    fs::write(
        &script,
        "set -euo pipefail\nIFS=$'\\n\\t'\n\
         f() {\n  local v=ok\n  echo \"$v\"\n}\nf\n",
    )
    .unwrap();
    let out = run(&["run", "--top", "0", script.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("ok"));
    assert!(text.contains("(exit 0)"));
    assert!(
        text.contains("1x  f"),
        "function attribution under strict mode:\n{text}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_traces_subshells_and_command_substitution() {
    let dir = tempdir("run-subshell");
    let script = dir.join("sub.sh");
    fs::write(
        &script,
        "v=$(printf inner)\n( echo \"in subshell $v\" )\necho outer\n",
    )
    .unwrap();
    let out = run(&["run", "--top", "0", script.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("in subshell inner"));
    // Both the substitution and the subshell body appear as traced lines.
    assert!(text.contains("sub.sh:1"));
    assert!(text.contains("sub.sh:2"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_keeps_the_raw_trace_with_out_and_report_replays_it() {
    let dir = tempdir("run-out");
    let script = dir.join("job.sh");
    let trace = dir.join("job.trace");
    fs::write(&script, "a=1\nb=2\nc=3\n").unwrap();
    let out = run(&[
        "run",
        "--out",
        trace.to_str().unwrap(),
        script.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("raw trace kept at"));
    assert!(trace.is_file());

    // Offline replay of the same trace sees the same commands.
    let replay = run(&["report", "--top", "0", trace.to_str().unwrap()]);
    assert!(replay.status.success());
    let text = stdout(&replay);
    assert!(text.contains("3 commands traced"));
    assert!(text.contains("job.sh:1"));
    assert!(text.contains("job.sh:3"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn report_on_fixture_trace_computes_exact_numbers() {
    let dir = tempdir("report-fixture");
    let trace = dir.join("fixed.trace");
    fs::write(&trace, fixture_trace()).unwrap();
    let out = run(&["report", "--top", "0", trace.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("./job.sh (exit 0)"), "{text}");
    assert!(text.contains("total 2.000s wall, 5 commands traced, 1 source file"));
    // job.sh:11 ran 101.4 -> 101.9 = 1.5s = 75.0% of 2.0s.
    assert!(text.contains("1.500s"), "{text}");
    assert!(text.contains("75.0%"), "{text}");
    // job.sh:2 ran 100.1 -> 100.2 = 100ms = 5.0%.
    assert!(text.contains("100ms"), "{text}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn collapse_on_fixture_trace_is_byte_exact() {
    let dir = tempdir("collapse-fixture");
    let trace = dir.join("fixed.trace");
    fs::write(&trace, fixture_trace()).unwrap();
    let out = run(&["collapse", trace.to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        "main;job.sh:2 100000\n\
         main;job.sh:5 100000\n\
         main;job.sh:6 100000\n\
         main;work;job.sh:10 100000\n\
         main;work;job.sh:11 1500000\n"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn report_json_on_fixture_trace_has_exact_fields() {
    let dir = tempdir("json-fixture");
    let trace = dir.join("fixed.trace");
    fs::write(&trace, fixture_trace()).unwrap();
    let out = run(&["report", "--json", trace.to_str().unwrap()]);
    assert!(out.status.success());
    let json = stdout(&out);
    assert!(json.contains("\"script\": \"./job.sh\""));
    assert!(json.contains("\"exit_code\": 0"));
    assert!(json.contains("\"total_us\": 2000000"));
    assert!(json.contains("\"commands\": 5"));
    assert!(json.contains(
        "{\"file\": \"./job.sh\", \"line\": 11, \"count\": 1, \"self_us\": 1500000, \"max_us\": 1500000, \"command\": \"step_b\"}"
    ));
    assert!(
        json.contains("{\"name\": \"work\", \"calls\": 1, \"commands\": 2, \"self_us\": 1600000}")
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn annotate_renders_gutter_times_against_the_source() {
    let dir = tempdir("annotate");
    let script = dir.join("job.sh");
    // Line numbers must match the fixture trace (lines 2, 5, 6, 10, 11).
    fs::write(
        &script,
        "#!/usr/bin/env bash\nsetup\n\n\nwork\necho done\n\n\nwork() {\nstep_a\nstep_b\n}\n",
    )
    .unwrap();
    let trace = dir.join("fixed.trace");
    fs::write(&trace, fixture_trace()).unwrap();
    let out = run(&[
        "annotate",
        "--script",
        script.to_str().unwrap(),
        trace.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[1].starts_with("        -      -"),
        "shebang untouched: {}",
        lines[1]
    );
    assert!(lines[2].contains("100ms") && lines[2].contains("setup"));
    assert!(lines[11].contains("1.500s") && lines[11].contains("step_b"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn annotate_without_script_flag_uses_the_recorded_path() {
    let dir = tempdir("annotate-auto");
    let script = dir.join("auto.sh");
    fs::write(&script, "echo one\n").unwrap();
    let trace = dir.join("auto.trace");
    let content = [
        format!("{US}START{US}50.000000{US}{}", script.display()),
        trace_line(
            1,
            "50.000000",
            script.to_str().unwrap(),
            1,
            "",
            "",
            "echo one",
        ),
        format!("{US}END{US}50.250000{US}0"),
    ]
    .join("\n");
    fs::write(&trace, content).unwrap();
    let out = run(&["annotate", trace.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("250ms") && text.contains("echo one"),
        "{text}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn report_rejects_a_file_that_is_not_a_trace() {
    let dir = tempdir("not-a-trace");
    let bogus = dir.join("notes.txt");
    fs::write(&bogus, "just some text\nno markers here\n").unwrap();
    let out = run(&["report", bogus.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("does not look like a bashprof trace"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_json_output_is_emitted_after_the_script_output() {
    let dir = tempdir("run-json");
    let script = dir.join("j.sh");
    fs::write(&script, "echo payload\n").unwrap();
    let out = run(&["run", "--json", script.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    let payload = text.find("payload").expect("script output present");
    let json = text.find("\"tool\": \"bashprof\"").expect("json present");
    assert!(payload < json);
    assert!(text.contains("\"exit_code\": 0"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_survives_a_script_that_installs_its_own_exit_trap() {
    // `trap` is last-writer-wins in bash: a user EXIT trap replaces the
    // bootstrap's END-record trap. bashprof must backfill the record so the
    // exit code and totals stay real instead of "?".
    let dir = tempdir("run-trap");
    let script = dir.join("trap.sh");
    fs::write(
        &script,
        "tmp=$(mktemp)\ntrap 'rm -f \"$tmp\"' EXIT\necho using \"$tmp\" > /dev/null\nexit 5\n",
    )
    .unwrap();
    let out = run(&["run", script.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(5));
    let text = stdout(&out);
    assert!(
        text.contains("(exit 5)"),
        "exit code must be backfilled:\n{text}"
    );
    assert!(!text.contains("(exit ?)"));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_leaves_script_stderr_clean_of_trace_noise() {
    // BASH_XTRACEFD keeps xtrace off stderr: a script that captures its own
    // stderr must not see PS4 lines.
    let dir = tempdir("run-stderr");
    let script = dir.join("s.sh");
    fs::write(&script, "echo warn >&2\necho ok\n").unwrap();
    let out = run(&["run", script.to_str().unwrap()]);
    assert!(out.status.success());
    let err = stderr(&out);
    assert!(err.contains("warn"));
    assert!(!err.contains('\u{1f}'), "trace leaked to stderr: {err}");
    fs::remove_dir_all(&dir).unwrap();
}
