# SystemVerilog Support

Celox can simulate synchronous RTL written in SystemVerilog through its **Rust
API**. Enable the `systemverilog` Cargo feature, supply your source files and
choose a top module. The TypeScript package and Vite plugin currently load
Veryl only.

The frontend supports a subset of SystemVerilog, including modules, memories,
interfaces and two-state or four-state simulation. It shares Celox's backends
and runtime with Veryl. See the [language reference](./systemverilog-reference)
when checking a particular construct.

## Start with a module

Enable the feature in your Rust project:

```toml
[dependencies]
celox = { version = "*", features = ["systemverilog"] }
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
| Advance simulation time and run `initial` and `always` processes with timing controls | `Simulation::from_sv_sources` |
| Include both Veryl and SystemVerilog sources | `Simulator::from_mixed_sources` |

`Simulator::tick` drives the rising edge of the selected event. `Simulation`
tracks time and runs scheduled edges. Constant initialization applies to both.
`initial` blocks that read design state or call system tasks, processes with
timing controls and `always` blocks with a sensitivity list other than `@*` run
as processes and need `Simulation`; a `Simulator` does not run them
([#1712](https://github.com/celox-sim/celox/issues/1712)).

Use `.four_state(true)` on the builder when your test needs X/Z values. The
[4-state guide](./four-state) explains the state model. For time units,
`$time`, `$stop` and memory loading, see the
[runtime reference](./systemverilog-reference#semantics-worth-knowing).

## What is supported

The main supported areas are:

| Area | Examples |
| --- | --- |
| Design structure | Modules, hierarchy, parameters, generate blocks, packages, interfaces and modports |
| Data | Packed vectors, multidimensional packed arrays, structs, enums and unpacked memories |
| RTL processes | `assign`, `always_comb`, `always @*`, blocking `always_latch`, edge-triggered `always` and `always_ff` |
| Timed processes | `initial` and `always` with `#delay` (also time literals such as `#5ns`), `@(...)` on variables, edges, selects, expressions and `@*`, `wait (...)`, nonblocking assignments and task calls with timing controls |
| Computation | Arithmetic, selects, assignment patterns, branches, loops and functions |
| Simulation helpers | Immediate assertions, display tasks, `$readmemh` / `$readmemb`, time queries and random numbers |

Each area has restrictions. Use the
[detailed support list](./systemverilog-reference#what-is-supported) to check
ports, types, statements or system functions before porting a design.

## What is not supported

Common reasons a design needs changes are:

- Class-based verification and concurrent assertions.
- `fork` / `join`, named events, intra-assignment timing such as `a = #1 b`,
  `iff` in event controls ([#1444](https://github.com/celox-sim/celox/issues/1444)) and real-valued delays
  ([#1707](https://github.com/celox-sim/celox/issues/1707)).
- Nonblocking latch assignments, multiple drivers and tri-state buses. Inferred latches in `always_comb` require `inferred_latch = "warn"` or `"allow"` in the common `[diagnostics]` configuration.
- Non-ANSI module ports, wildcard `.*` connections, gate primitives and `bind`.
- `real`, unpacked structs and several interface features.

The [complete limitations list](./systemverilog-reference#what-is-not-supported)
includes narrower restrictions and tracking issues. Unsupported constructs
produce a diagnostic naming the construct and its tracking issue.

## Mix SystemVerilog and Veryl

Either language may be the top, and a hierarchy may cross the boundary in both
directions. Use `Simulator::from_mixed_sources` to supply both source lists.
Veryl refers to an SV module as `$sv::Module`; SV uses a Veryl module's emitted
name, such as `prj_Module`.

Interfaces and `input` / `output` modports can also cross the boundary.
See [mixed-language designs](./systemverilog-reference#mixed-language) for
naming, interface parameters and cross-language restrictions.

## DPI-C imports

Enable `sv-dpi` to call supported C functions from clocked SV processes.
Register functions or shared libraries with `dpi_function` or `dpi_library`;
these are `unsafe` APIs because the C signatures must match the SV imports.
See [DPI-C imports](./systemverilog-reference#dpi-c-imports) for a complete example
and the supported types and call sites.

## Find a specific behavior {#semantics-worth-knowing}

- [Initialization, scopes, variable lifetimes and memory files](./systemverilog-reference#semantics-worth-knowing)
- [Event and level waits in processes](./systemverilog-reference#timing-semantics)
- [Time units, precision and time queries](./systemverilog-reference#time-and-precision)
- [Random seeds and reproducibility](./systemverilog-reference#random-numbers)

## SystemVerilog emitted by Veryl

The Veryl conformance suite also checks the SystemVerilog emitted by Veryl.
Some cases cannot be emitted or have different language semantics; see the
[remaining exclusions](./systemverilog-reference#systemverilog-emitted-by-veryl).
