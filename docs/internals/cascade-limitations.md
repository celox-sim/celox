# Runtime Semantics

Celox uses event-driven execution with split evaluate and commit phases. This page
defines the runtime ordering model for simultaneous and cascaded clock events.

## Simultaneous domains

All clock domains triggered at one simulation time observe the same committed
state. Celox first evaluates their next-state logic into the Working region and
only then commits all results to the Stable region.

```veryl
assign gclk = clk;

always_ff (clk) {
    cnt1 = cnt1 + 1;
}

always_ff (gclk) {
    cnt2 = cnt2 + cnt1;
}
```

When `clk` and `gclk` trigger together, `cnt2` reads the pre-update value of
`cnt1`. Evaluation order therefore does not change the result.

## Cascaded clocks

A sequential update may itself create another clock edge:

```veryl
always_ff (clk) {
    clk_div = ~clk_div;
}

always_ff (clk_div) {
    cnt = cnt + 1;
}
```

After committing the first set of domains, Celox propagates combinational logic
and examines triggered bits. Newly discovered domains are evaluated in another
round of the same simulation step. The loop ends when no new event-producing
change remains.

Conceptually, each round is:

1. discover triggered domains;
2. evaluate every domain from the current Stable region;
3. commit their Working values together;
4. settle combinational logic and discover further triggers.

The runtime may use a combined evaluate-and-commit kernel when exactly one domain
can run without affecting cascade consistency. This is an implementation
optimization and does not change the ordering model above.

## Boundaries

- Combinational dependency cycles, including zero-delay cycles between clocks,
  are rejected during simulator construction.
- Celox models RTL event behavior, not gate delays or a general-purpose
  SystemVerilog delta-cycle scheduler.

User-facing guidance for dependency cycles is in
[Combinational Loops](/guide/combinational-loops).
