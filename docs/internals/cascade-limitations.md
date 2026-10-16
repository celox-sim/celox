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

Only a copy of a whole variable joins its source's domain. A clock taken from
part of a wider signal, such as `let divided: '_ clock = counter[0];`, is a
derived clock of its own and is edge-sensed on the selected bit.

## Gated clocks

A clock computed combinationally from another clock's signal changes at the
same instant as that signal:

```veryl
let gated: '_ clock = clk & en;

always_ff (clk) {
    cnt1 = cnt1 + 1;
    en = en_next;
}

always_ff (gated) {
    cnt2 = cnt1;
}
```

Its edge is judged when `clk` changes, from the state before the edge with
`clk` at its new level, and its domain evaluates together with `clk`'s as a
simultaneous domain: `cnt2` reads the pre-update `cnt1`, and the edge is gated
by the `en` from before it. A change of the gate caused by that commit, such as
`en` rising in the same step, is not an edge of `gated` (the latch of a
clock-gating cell holds the enable while the clock is high). SystemVerilog
schedulers re-evaluate `clk & en` after the nonblocking updates and report such
a rise as a separate edge, so designs that depend on it differ.

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

## Event-based ticks

`Simulator::tick` applies the same rules to one active edge of an event.
Gated clocks of the event are judged from the state before the edge and
evaluate with the ticked domain; after the commit, the rounds above run the
domains whose clocks are computed from state. `Simulator::tick_events` does
the same for coincident events, and the native testbench's `clock.next()` and
the N-API `tick` use the same mechanism.

A tick stands for one clock cycle without simulation time. Clocks that react
to the event's signal returning to its inactive level, such as `~clk`, run
after the active edge's rounds and read the state they left, as the second half
of the cycle. A tick does not change the event's signal.

Which derived domains an event can reach is computed once when the design is
built, together with a small program for each gate where its combinational
cone allows: judging a gate then reads only the state it depends on instead of
settling the design. Larger cones, and four-state designs, settle the design
with the signal at each level instead. An event that reaches no derived domain
takes the plain tick path with no added work.

## Boundaries

- Combinational dependency cycles, including zero-delay cycles between clocks,
  are rejected during simulator construction.
- Celox models RTL event behavior, not gate delays or a general-purpose
  SystemVerilog delta-cycle scheduler.

User-facing guidance for dependency cycles is in
[Combinational Loops](/guide/combinational-loops).
