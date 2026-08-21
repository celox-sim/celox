# Heliodor Linux Benchmark

Heliodor is Celox's large external Veryl workload. It boots a pinned Linux image
and compares Celox's native backend with synchronous `veryl-cc` execution using
the same design revision and input workload. Cranelift Linux boot measurements
are not collected or published because their runtime is outside the useful scale
of this comparison.

## What the benchmark answers

The benchmark separates two questions:

1. How long does Celox take to compile the design?
2. How quickly does the generated simulator execute the complete workload?

Only the second measurement is used for generated-code throughput comparisons.
A partial boot, projected completion time, or compile-only result is not a
successful execution result.

## Valid result

A run is accepted only when it:

- uses the pinned Heliodor and workload revisions;
- reaches the configured Linux completion marker;
- records compilation and execution separately;
- compares runners built from the intended Celox and Veryl revisions;
- preserves the logs needed to diagnose a timeout or semantic mismatch.

This fixed completion marker prevents faster failures or incomplete boots from
being reported as performance improvements.

## Run locally

```bash
bash scripts/run-heliodor-bench.sh run
```

The first run needs network access to obtain the pinned Heliodor checkout. The
script prints the selected revisions, build configuration, completion status,
and timings. Use the same machine and configuration for before/after comparisons.

Published results appear in the **Heliodor Linux** section of the
[benchmark dashboard](./index.md).
