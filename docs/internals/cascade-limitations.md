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

Celox follows the SystemVerilog scheduling of the code Veryl emits (IEEE
1800-2023 4.4.2, 4.5), for both frontends. When `clk` rises, `clk & en` is
evaluated in the same active region as `clk`'s processes, before the
nonblocking updates: its edge is judged from the state before the edge with
`clk` at its new level, and its domain evaluates together with `clk`'s as a
simultaneous domain. `cnt2` reads the pre-update `cnt1`, and the edge is gated
by the `en` from before it.

The nonblocking updates then re-evaluate the gate. If the edge raised `en`
while `clk` is still high, `gated` rises after the commit; that is a further
edge, handled in the cascade rounds below, and its domain reads the committed
state. A gate that falls after the commit is a falling edge in the same way.
When `clk` returns low, clocks such as `~clk` or a falling edge of `gated` are
judged from the state the rising edge left, as a separate instant of the timed
`Simulation`.

Veryl's own simulator holds a gate's enable for the whole edge, like a
clock-gating cell, and does not report the rise after the commit. Celox does
not follow it there; the shared suite records the difference for that
simulator.

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

`Simulator::posedge`, `negedge`, `active_edge`, `inactive_edge` and the
coincident `edges` apply the same rules to the transition of an event signal
they make, as a timed step does: the event's own domain runs on its active
edge, gated clocks of either polarity are judged at the transition, and the
signal stays at its new level. Gated clocks are judged from the state before
the edge and evaluate with the event's own domain; after the commit, the
rounds above run the domains whose clocks are computed from state and the
gated clocks the commit moves.

`Simulator::tick` is one clock period, the active edge followed by the
inactive edge, and is equivalent to the two timed steps of that period;
`Simulator::tick_events` does the same for coincident events, and the
native testbench's `clock.next()` takes the same two edges. The
[execution semantics](../guide/execution-semantics.md) define them by
SystemVerilog. The N-API handles and the WASM bridge of the TypeScript
package use the same plans.

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
