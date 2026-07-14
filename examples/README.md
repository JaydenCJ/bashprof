# bashprof examples

Runnable scripts to try the profiler on.

## ci-setup.sh

A miniature CI setup script (dependency fetch, asset compile, cache warm)
whose slow lines are not the ones you would guess from reading it.

```bash
# hot-line report
bashprof run examples/ci-setup.sh

# keep the raw trace, then render a flamegraph from it
bashprof run --out /tmp/ci.trace examples/ci-setup.sh
bashprof collapse /tmp/ci.trace > /tmp/ci.folded
# feed /tmp/ci.folded to flamegraph.pl, inferno-flamegraph or speedscope

# per-line source annotation (also a cheap coverage view)
bashprof annotate /tmp/ci.trace
```

Things worth noticing in the output:

- `sleep 0.12` inside the `fetch_deps` loop accumulates across 3 iterations
  and beats the single `sleep 0.35` — exactly the kind of fact that is
  invisible without per-line aggregation.
- `warm_cache` runs 50 loop iterations yet costs almost nothing; a high
  `COUNT` with a low `SELF` tells you not to bother optimizing it.
- The `FUNCTIONS` section shows one call each, with self-time excluding
  callees, so the numbers add up instead of double counting.
