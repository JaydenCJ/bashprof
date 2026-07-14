//! Collapsed-stack output — the interchange format flamegraph tooling eats.
//!
//! One line per distinct stack: `frame;frame;...;leaf <value>`, where the
//! value is accumulated self-time in **microseconds**. The innermost frame
//! is the `file:line` of the traced command, so the rendered flamegraph is
//! line-level, not just function-level. Feed it straight to `flamegraph.pl`,
//! `inferno-flamegraph`, or drop it into speedscope.

use crate::profile::Profile;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Render the profile as collapsed stacks.
///
/// Identical stacks are merged (values summed). Output is sorted
/// lexicographically by stack so it is byte-stable across runs of the same
/// trace. Zero-value stacks are kept: a line that executed matters even if
/// it was too fast for the clock to see.
pub fn render(profile: &Profile) -> String {
    let mut folded: BTreeMap<String, u64> = BTreeMap::new();
    for sample in &profile.samples {
        let mut key = String::new();
        for frame in &sample.stack {
            key.push_str(&sanitize(frame));
            key.push(';');
        }
        let file = basename(&sample.file);
        let _ = write!(key, "{}:{}", sanitize(&file), sample.line);
        *folded.entry(key).or_insert(0) += sample.self_us;
    }
    let mut out = String::new();
    for (stack, us) in folded {
        let _ = writeln!(out, "{stack} {us}");
    }
    out
}

/// Frame names must not contain the two structural characters of the
/// format: `;` (frame separator) and space (value separator).
fn sanitize(frame: &str) -> String {
    frame
        .chars()
        .map(|c| match c {
            ';' => ':',
            ' ' | '\t' | '\n' => '_',
            c => c,
        })
        .collect()
}

/// Long absolute paths make unreadable flamegraph frames; the file name is
/// enough to identify the source (the report keeps the full path).
fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Sample;

    fn sample(stack: &[&str], file: &str, line: u32, us: u64) -> Sample {
        Sample {
            stack: stack.iter().map(|s| s.to_string()).collect(),
            file: file.into(),
            line,
            self_us: us,
        }
    }

    fn profile_with(samples: Vec<Sample>) -> Profile {
        Profile {
            samples,
            ..Profile::default()
        }
    }

    #[test]
    fn identical_stacks_merge_and_output_is_sorted_deterministically() {
        let p = profile_with(vec![
            sample(&["main", "zeta"], "job.sh", 9, 40),
            sample(&["main", "alpha"], "job.sh", 2, 100),
            sample(&["main", "alpha"], "job.sh", 2, 250),
        ]);
        assert_eq!(
            render(&p),
            "main;alpha;job.sh:2 350\nmain;zeta;job.sh:9 40\n"
        );
    }

    #[test]
    fn distinct_lines_in_the_same_function_stay_separate() {
        // Line-level resolution is the whole point: two hot spots inside
        // one function must not blur together.
        let p = profile_with(vec![
            sample(&["main", "work"], "job.sh", 5, 100),
            sample(&["main", "work"], "job.sh", 6, 200),
        ]);
        assert_eq!(
            render(&p),
            "main;work;job.sh:5 100\nmain;work;job.sh:6 200\n"
        );
    }

    #[test]
    fn file_paths_are_reduced_to_basenames_in_frames() {
        let p = profile_with(vec![sample(&["main"], "/opt/ci/steps/build.sh", 12, 7)]);
        assert_eq!(render(&p), "main;build.sh:12 7\n");
    }

    #[test]
    fn structural_characters_in_frame_names_are_sanitized() {
        // Function names are user input; a `;` or space would corrupt the
        // folded format silently.
        let p = profile_with(vec![sample(&["main", "odd;name x"], "a b.sh", 1, 9)]);
        assert_eq!(render(&p), "main;odd:name_x;a_b.sh:1 9\n");
    }

    #[test]
    fn zero_value_stacks_are_kept() {
        // Sub-microsecond commands still executed; dropping them would make
        // the flamegraph lie about coverage.
        let p = profile_with(vec![sample(&["main"], "job.sh", 1, 0)]);
        assert_eq!(render(&p), "main;job.sh:1 0\n");
    }

    #[test]
    fn empty_profile_renders_empty_output() {
        assert_eq!(render(&profile_with(vec![])), "");
    }
}
