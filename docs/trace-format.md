# bashprof raw trace format

`bashprof run --out FILE` keeps the raw trace, which `report`, `collapse`
and `annotate` can replay offline. The format is what bash's xtrace mode
emits under bashprof's `PS4`, plus two control records — it is designed to
be greppable with plain tools, not a binary format.

## Line kinds

Fields are separated by the ASCII **unit separator** (`0x1F`, shown as `␟`
below). That byte cannot appear in timestamps or line numbers, does not
occur in sane paths, and bash's xtrace quoting never emits it on its own.

### 1. Control records (written by the bootstrap)

```text
␟START␟<epochrealtime>␟<script path>
␟END␟<epochrealtime>␟<exit code>
```

`START` opens the trace. `END` is written by an EXIT trap; because `trap`
is last-writer-wins in bash, a profiled script that installs its own EXIT
trap replaces that trap — in that case `bashprof run` backfills the `END`
record itself from the same wall clock after the process exits.

### 2. Events (one per traced simple command)

```text
+␟<ts>␟<source>␟<lineno>␟<funcname>␟<funcdepth>␟<command>
```

| Field | PS4 expansion | Meaning |
|---|---|---|
| leading `+` | (PS4's first char) | Repeated once per subshell nesting level |
| `ts` | `${EPOCHREALTIME}` | Wall clock, microsecond resolution, locale radix (`.` or `,`) |
| `source` | `${BASH_SOURCE[0]:-}` | File the command came from |
| `lineno` | `${LINENO}` | 1-based line in `source` |
| `funcname` | `${FUNCNAME[0]:-}` | Innermost function, empty at top level |
| `funcdepth` | `${FUNCNAME[@]+${#FUNCNAME[@]}}` | Frame count; empty (=0) at top level |
| `command` | (appended by bash) | The command as xtrace prints it |

Why name + depth instead of the whole `${FUNCNAME[*]}` stack: `[*]` joins
array elements with the first character of `IFS`, and "strict mode" scripts
set `IFS=$'\n\t'` — which would split every trace line. The profiler
reconstructs the full stack from depth transitions instead, which is
IFS-proof.

### 3. Continuations

xtrace prints commands verbatim, so a command containing an embedded
newline spills onto lines with no marker. The parser appends them to the
previous event's command text.

## Attribution model

Each event's **self-time** is the gap to the next event — the time bash
spent executing that command (including any external process it spawned)
before reaching the next one. The final event is closed by `END`. A line
that calls a function is charged only for the dispatch; the callee's lines
carry their own time, and the collapsed output keeps the whole stack so
flamegraphs show inclusive time without double counting.

Interleaved writes from background jobs can produce out-of-order
timestamps; negative gaps are clamped to zero rather than corrupting
totals.

## Filtered events

The parser drops bootstrap-internal noise: events whose `source` is empty
(the `. "$0"` dispatch inside `bash -c`), and anything whose function or
command starts with `__bashprof`.

## JSON output

`report --json` emits a single object with integer-microsecond times:
`tool`, `version`, `script`, `exit_code`, `total_us`, `commands`,
`skipped_lines`, `files[]`, `lines[]` (`file`, `line`, `count`, `self_us`,
`max_us`, `command`) and `functions[]` (`name`, `calls`, `commands`,
`self_us`). The layout is stable within a minor version.
