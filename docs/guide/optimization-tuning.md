# Optimization Tuning

The default optimization level is intended for normal use. Change it only when
compile time or simulation throughput is a measured bottleneck in your design.

## Optimization levels

| Level | Use it when | Trade-off |
|---|---|---|
| `O0` | Fast edit-and-run iterations matter most | Faster compilation, slower simulation |
| `O1` | General-purpose testing | Default balance |
| `O2` | Long-running tests only need top-level ports | Enables dead-store elimination |

```typescript
import { Simulator } from "@celox-sim/celox";

const normal = Simulator.create(module); // O1
const quickBuild = Simulator.create(module, { optLevel: "O0" });
const throughput = Simulator.create(module, { optLevel: "O2" });
```

`O2` preserves top-level ports but may remove internal or child-instance state
that is not otherwise observable. Read [Dead Store Elimination](./dead-store-elimination.md)
before enabling it in tests that inspect hierarchy.

## How to choose

Measure the complete workload you care about:

1. measure simulator creation separately from test execution;
2. run the same design, inputs, and number of cycles for every level;
3. include any hierarchy reads or waveform dumping used by the real test;
4. repeat enough times to distinguish a stable difference from machine noise.

Use `O0` for iteration only if shorter compilation outweighs slower execution.
Use `O2` only if its observability policy matches the test. Otherwise, keep the
default `O1`.

Per-pass overrides and backend-specific compiler controls exist for Celox
development and performance investigation. They are intentionally not part of
the recommended user workflow; their architecture is documented under
[Optimization Architecture](/internals/optimizations).
