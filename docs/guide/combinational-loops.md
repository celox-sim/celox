# Combinational Loops

Celox performs static dependency analysis on `always_comb` blocks and schedules them in topological order. When it detects a cycle in the dependency graph, compilation fails with a `CombinationalLoop` error.

## False Loops

A **false loop** is a cycle that appears in the static dependency graph but can never actually loop at runtime. The most common cause is a mux whose two branches each depend on the opposite path:

```veryl
module Top (
    sel: input  logic,
    i:   input  logic<2>,
    o:   output logic<2>,
) {
    var v: logic<2>;
    always_comb {
        if sel {
            v[0] = v[1];  // reads v[1]
            v[1] = i[1];
        } else {
            v[0] = i[0];
            v[1] = v[0];  // reads v[0]
        }
    }
    assign o = v;
}
```

`v[0]` and `v[1]` appear to depend on each other, but `v[0]→v[1]` only happens when `sel=1` and `v[1]→v[0]` only when `sel=0` — they never loop simultaneously.

Without intervention, this fails to compile. Use `falseLoops` to declare the cycle safe:

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  falseLoops: [
    { from: "v", to: "v" },
  ],
});
```

The `from` and `to` fields identify the signals involved in the cycle. Celox will execute the SCC block multiple times (the exact count is derived from the structural depth of the cycle) to ensure all values propagate correctly regardless of execution order.


## Signal Path Syntax

`from` and `to` accept a signal path string:

| Pattern | Meaning |
|---------|---------|
| `"v"` | Top-level variable `v` |
| `"u_sub:i_data"` | Port `i_data` of child instance `u_sub` |
| `"u_a.u_b:x"` | Port `x` of instance `u_b` inside `u_a` |

## Further Reading

- [Combinational Analysis](/internals/combinational-analysis) -- How the dependency graph is built and scheduled.
- [Writing Tests](./writing-tests.md) -- Simulator options overview.
