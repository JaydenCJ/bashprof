//! Minimal JSON emission — just enough to serialize a [`Profile`] without
//! pulling in a serializer dependency. Emission only; bashprof never parses
//! JSON.

use crate::profile::Profile;
use std::fmt::Write as _;

/// Escape a string per RFC 8259 (quotes, backslash, control characters).
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Serialize a profile as a single JSON object (pretty, 2-space indent).
///
/// Layout is stable and documented in `docs/trace-format.md`; times are
/// integer microseconds so consumers never deal with float rounding.
pub fn render(profile: &Profile) -> String {
    let mut o = String::new();
    o.push_str("{\n");
    let _ = writeln!(o, "  \"tool\": \"bashprof\",");
    let _ = writeln!(o, "  \"version\": \"{}\",", crate::VERSION);
    match &profile.script {
        Some(s) => {
            let _ = writeln!(o, "  \"script\": \"{}\",", escape(s));
        }
        None => {
            let _ = writeln!(o, "  \"script\": null,");
        }
    }
    match profile.exit_code {
        Some(rc) => {
            let _ = writeln!(o, "  \"exit_code\": {rc},");
        }
        None => {
            let _ = writeln!(o, "  \"exit_code\": null,");
        }
    }
    let _ = writeln!(o, "  \"total_us\": {},", profile.total_us);
    let _ = writeln!(o, "  \"commands\": {},", profile.commands);
    let _ = writeln!(o, "  \"skipped_lines\": {},", profile.skipped);

    let files: Vec<String> = profile
        .files
        .iter()
        .map(|f| format!("\"{}\"", escape(f)))
        .collect();
    let _ = writeln!(o, "  \"files\": [{}],", files.join(", "));

    o.push_str("  \"lines\": [\n");
    for (i, l) in profile.lines.iter().enumerate() {
        let comma = if i + 1 < profile.lines.len() { "," } else { "" };
        let _ = writeln!(
            o,
            "    {{\"file\": \"{}\", \"line\": {}, \"count\": {}, \"self_us\": {}, \"max_us\": {}, \"command\": \"{}\"}}{comma}",
            escape(&l.file),
            l.line,
            l.count,
            l.self_us,
            l.max_us,
            escape(&l.command)
        );
    }
    o.push_str("  ],\n");

    o.push_str("  \"functions\": [\n");
    for (i, f) in profile.funcs.iter().enumerate() {
        let comma = if i + 1 < profile.funcs.len() { "," } else { "" };
        let _ = writeln!(
            o,
            "    {{\"name\": \"{}\", \"calls\": {}, \"commands\": {}, \"self_us\": {}}}{comma}",
            escape(&f.name),
            f.calls,
            f.commands,
            f.self_us
        );
    }
    o.push_str("  ]\n");
    o.push_str("}\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{FuncStat, LineStat};

    #[test]
    fn escape_covers_rfc8259_specials_and_leaves_utf8_intact() {
        assert_eq!(escape("echo hello"), "echo hello");
        assert_eq!(escape(r#"grep "a\b""#), r#"grep \"a\\b\""#);
        assert_eq!(escape("a\nb\tc\rd"), "a\\nb\\tc\\rd");
        // Other control chars use \u00XX — ANSI escapes appear in real
        // traced commands.
        assert_eq!(escape("\u{1b}[0m"), "\\u001b[0m");
        assert_eq!(escape("\u{1f}"), "\\u001f");
        assert_eq!(escape("実行 ✓"), "実行 ✓");
    }

    fn sample_profile() -> Profile {
        Profile {
            script: Some("job.sh".into()),
            exit_code: Some(0),
            total_us: 1234,
            commands: 2,
            files: vec!["job.sh".into()],
            lines: vec![LineStat {
                file: "job.sh".into(),
                line: 3,
                count: 2,
                self_us: 1000,
                max_us: 900,
                command: "echo \"hi\"".into(),
            }],
            funcs: vec![FuncStat {
                name: "main".into(),
                calls: 0,
                commands: 2,
                self_us: 1000,
            }],
            samples: vec![],
            skipped: 0,
        }
    }

    #[test]
    fn render_contains_all_top_level_keys() {
        let json = render(&sample_profile());
        for key in [
            "\"tool\"",
            "\"version\"",
            "\"script\"",
            "\"exit_code\"",
            "\"total_us\"",
            "\"commands\"",
            "\"skipped_lines\"",
            "\"files\"",
            "\"lines\"",
            "\"functions\"",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }

    #[test]
    fn render_escapes_command_text() {
        let json = render(&sample_profile());
        assert!(json.contains(r#""command": "echo \"hi\"""#));
    }

    #[test]
    fn render_uses_null_for_missing_script_and_exit_code() {
        let mut p = sample_profile();
        p.exit_code = None;
        p.script = None;
        let json = render(&p);
        assert!(json.contains("\"exit_code\": null"));
        assert!(json.contains("\"script\": null"));
    }

    #[test]
    fn render_is_structurally_balanced() {
        // Cheap structural sanity: balanced braces/brackets and no trailing
        // comma before a closing bracket.
        let json = render(&sample_profile());
        assert_eq!(
            json.matches('{').count(),
            json.matches('}').count(),
            "unbalanced braces"
        );
        assert_eq!(json.matches('[').count(), json.matches(']').count());
        assert!(!json.contains(",\n  ]"));
        assert!(!json.contains(",\n}"));
    }
}
