# SystemVerilog Support

Celox can simulate SystemVerilog through its **Rust API**.
Supply your source files and choose a top module. The TypeScript package and Vite plugin currently load
Veryl only.

Celox follows IEEE Std 1800™-2023.
Instead of restating the standard, the
[deviations page](./systemverilog-reference) lists only what is unsupported,
what intentionally differs, and which implementation-defined choices Celox
makes. Anything not listed there behaves as the standard specifies; a
difference you cannot find there is a bug.

## Start with a module

Add Celox to your Rust project:

```toml
[dependencies]
celox = "*"
```

Pass the source text and its path to `Simulator::from_sv_sources`, followed by
the name of the top module:

```rust
use celox::Simulator;
use std::path::Path;

let source = r#"
    module Counter(input logic clk, input logic rst, output logic [7:0] q);
        always_ff @(posedge clk) q <= rst ? 8'd0 : q + 8'd1;
    endmodule
"#;
let mut sim = Simulator::from_sv_sources(
    vec![(source, Path::new("counter.sv"))],
    "Counter",
)
.build()?;

let clk = sim.event("clk");
let rst = sim.signal("rst");
let q = sim.signal("q");
sim.modify(|io| io.set(rst, 1u8))?;
sim.tick(clk)?; // Reset the counter.
sim.modify(|io| io.set(rst, 0u8))?;
sim.tick(clk)?; // Count one rising edge.
assert_eq!(sim.get(q), 1u8.into());
```

Supply additional files in the same source list. Each file is a separate
compilation unit; choose the top by its SystemVerilog module name.

## Choose how to drive the design

| What your test needs | API |
| --- | --- |
| Set inputs, evaluate logic and advance clock events from Rust | `Simulator::from_sv_sources` |
| Advance simulation time, run timed processes and execute `final` procedures | `Simulation::from_sv_sources` |
| Include both Veryl and SystemVerilog sources | `Simulator::from_mixed_sources` |

`Simulator::tick` drives the rising edge of the selected event.
`Simulation` tracks time and runs scheduled edges.
A `Simulator` runs no process, so a design with timed processes,
non-constant `initial` blocks, or `final` procedures needs `Simulation`;
building a `Simulator` for one fails with
`SimulatorErrorKind::ProcessRequiresSimulation`.
See the [execution model](./systemverilog-reference#celox-execution-model)
for exactly which processes need `Simulation`.

Use `.four_state(true)` on the builder when your test needs X/Z values.
The [4-state guide](./four-state) explains the state model.
For a mixed hierarchy, build the `Simulation` with
`Simulation::from_sources(veryl).with_sv_sources(sv)`.

## Deviations from the standard

Unsupported constructs are rejected with an `Unsupported` error naming the
construct and its tracking issue; nothing is silently ignored.
The areas most likely to require changes when porting a design are:

- Concurrent assertions beyond the Boolean and fixed-delay sequence subset,
  and class randomization and constraints beyond the scalar subset.
- `bind`, checkers, covergroups, clocking blocks, `specify` blocks, UDPs,
  `defparam`, procedural `force` / `release`, and `disable` (other than
  `disable fork`).
- Nonblocking assignments in `always_comb` and `always_latch`, net drivers
  in several modules (tri-state buses across modules), and `iff` in
  `always_ff`.
- `ref` ports, non-ANSI port expressions, gate primitives with delays or
  strengths, and several interface features.
- Inferred latches in `always_comb`, which are an error unless
  `inferred_latch = "warn"` or `"allow"` is set.

The [deviations page](./systemverilog-reference#what-is-not-supported) is
the complete list, including intentional differences and
implementation-defined choices for random numbers, time defaults, and event
scheduling order.

## Mix SystemVerilog and Veryl

Either language may be the top, and a hierarchy may cross the boundary in
both directions. Use `Simulator::from_mixed_sources` to supply both source
lists. Veryl refers to an SV module as `$sv::Module`; SV uses a Veryl
module's emitted name, such as `prj_Module`. Interfaces and `input` /
`output` modports can also cross the boundary.
See [mixed-language designs](./systemverilog-reference#mixed-language).

## DPI-C imports

Procedural code can call C functions. Register
functions or shared libraries with `dpi_function` or `dpi_library`; these
are `unsafe` APIs because the C signatures must match the SV imports. The
`celox run` command links shared libraries with `--dpi-lib PATH`. See
[DPI-C imports](./systemverilog-reference#dpi-c-imports) for the supported
types and call sites.

## Find a specific behavior {#semantics-worth-knowing}

- [Unsupported constructs](./systemverilog-reference#what-is-not-supported)
- [Intentional deviations](./systemverilog-reference#intentional-deviations)
- [Event scheduling choices](./systemverilog-reference#timing-semantics)
- [Time units and precision defaults](./systemverilog-reference#time-and-precision)
- [Random seeds and reproducibility](./systemverilog-reference#random-numbers)
- [Termination and `final` procedures](./systemverilog-reference#celox-execution-model)

## SystemVerilog emitted by Veryl

The Veryl conformance suite also checks the SystemVerilog emitted by Veryl.
Some cases cannot be emitted or have different language semantics; see the
[remaining exclusions](./systemverilog-reference#systemverilog-emitted-by-veryl).
