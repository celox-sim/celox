# Cascade Clocks and Race Condition Handling

This document explains the resolution strategies and implementation details for cascade clocks (chained clocks) and the race conditions they can cause in Celox.

## 1. Consistency Guarantees for Cascade Clocks

The current implementation uses multi-phase evaluation to guarantee logical consistency when multiple clocks (and trigger signals) change at the same simulation time.

### Consistency in Combinational Cascades
Even when a change in clock `clk` drives another clock `gclk` through a combinational circuit (`assign`), the FF update timing is properly controlled.

```veryl
assign gclk = clk;

always_ff (clk) {
    cnt1 = cnt1 + 1;
}

always_ff (gclk) {
    cnt2 = cnt2 + cnt1; // Must correctly reference the "pre-update" value of cnt1
}
```

-   **Behavior**:
    1.  **Phase 1 (Discovery)**: Detect the edges of `clk` and `gclk`. Execute each FF block in `eval_only` (computation phase) and hold the results in a temporary area (Working Region). At this point, the computation of `cnt2` uses the value of `cnt1` from the not-yet-updated Stable region.
    2.  **Phase 2 (Apply)**: After all triggered domains have been evaluated, commit the results to the Stable region all at once.
    3.  **Phase 3 (Stabilize)**: Re-evaluate combinational circuits based on the updated values.

This guarantees "non-blocking assignment" behavior consistent with physical RTL semantics.

### Sequential Cascades (e.g., Clock Division)
When an FF output serves as a trigger for another FF (e.g., a clock divider), the trigger discovery loop handles this correctly.

```veryl
always_ff (clk) {
    clk_div = ~clk_div;
}

always_ff (clk_div) {
    cnt = cnt + 1;
}
```

-   **Behavior**:
    -   When the evaluation of `clk` causes `clk_div` to change, the "trigger discovery loop" within the main loop detects this and adds the `clk_div` domain to the execution list within the same simulation step.
    -   Thanks to multi-phase evaluation, even though the change in `clk_div` is visible, the update of `cnt` is synchronized with the updates of other signals driven by `clk`.

## 2. Verified Tests

These behaviors are verified in `tests/cascade_race.rs`, where all tests are confirmed to **PASS**.

-   `test_cascade_race_condition`: Verifies prevention of premature value capture in combinational cascades.
-   `test_sequential_cascade_race_condition`: Verifies correctness of trigger propagation in sequential cascades (divided clocks).

## 3. Implementation Details

1.  **Working Region (2-Region Memory)**: A Working region was introduced to temporarily hold computation results instead of applying them immediately.
2.  **Split Blocks (eval_only / apply)**: The JIT compiler generates FF blocks split into two execution units: "compute" and "update."
3.  **Trigger Discovery Loop**: Within a simulation step, evaluation and combinational propagation repeat until no signal change triggers a new domain.

## 4. Current Limitations

-   **Circular Dependencies (Zero-delay Loop)**: If a combinational loop exists between clocks, it is statically detected and rejected as a `CombinationalLoop` error at simulator build time (`Simulator::builder().build()`).
-   **Single-phase Optimization**: When only a single trigger fires in a simulation step and it is not a cascade target, the eval_only/apply split is skipped and `eval_apply_ff_at` is used for batch execution as an optimization. This decision is made on a per-step basis, not based on the overall design properties.
