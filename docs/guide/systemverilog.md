# SystemVerilog Support

Celox can simulate SystemVerilog through its **Rust API**.
Supply your source files and choose a top module. The TypeScript package and
Vite plugin currently load Veryl only.

Celox follows IEEE Std 1800™-2023.
Instead of restating the standard, the
[conformance and deviations page](./systemverilog-reference) separates
**unsupported features**, **partially supported subsets** (with Supported /
Not supported tables), **narrow limitations**, and **intentional deviations**.
Valid IEEE 1800-2023 code not listed there behaves as the standard specifies
(an unlisted difference is a bug). Default mode accepts several
warn-by-default compatibility relaxations; in strict mode (`strict = true` /
`--strict`), accepting unlisted non-IEEE 1800-2023 code is also a bug.

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

## Support overview and deviations

Unsupported constructs are rejected with an `Unsupported` error naming the
construct and its tracking issue; nothing is silently ignored.

| Category | Summary | Reference |
| --- | --- | --- |
| **Not supported** | `bind`, checkers, `specify` blocks, UDPs, `disable` (other than `disable fork`), `wait_order`, and unpacked/tagged unions | [Unsupported features](./systemverilog-reference#what-is-not-supported) |
| **Partially supported** | Side-by-side Supported / Not supported tables for ports/`ref` ports/nets/`defparam`/gates, hierarchical references, interfaces, virtual interfaces, programs, clocking blocks, unpacked structs, procedural `force`/`assign`, delays, event controls, classes, randomization, dynamic containers, concurrent assertions, covergroups (coverpoints with automatic bins and the bins of values: arrays, `default`, `ignore_bins`, `illegal_bins`, `wildcard`, guards and `with` filters; crosses with `binsof` and `intersect` selections; options and the predefined methods of covergroups, coverpoints and crosses; clocking events, also of embedded covergroups; without transition bins, cross `with` selections, and the clocking events of covergroups of interfaces), and I/O | [Partially supported features](./systemverilog-reference#partial-support) |
| **Narrow limitations** | Specific restrictions such as non-ANSI port expressions, nonblocking assignments in `always_comb`/`always_latch`, and loop unrolling bounds | [Narrow limitations](./systemverilog-reference#narrow-limitations) |
| **Deviations (standard code)** | Deliberate differences on valid IEEE 1800-2023 code: program continuous assignment region, cross-module interface static storage, procedural `assign` override timing, run-time `%p`, and `$exit` warnings | [Deviations affecting standard code](./systemverilog-reference#deviations-standard-code) |
| **Permissive non-standard acceptance** | Optional/warn-by-default relaxations (`strict = true` / `--strict` promotes warnings to errors) plus non-standard extensions such as variable `force` selects and `$psprintf` | [Permissive non-standard acceptance](./systemverilog-reference#deviations-nonstandard-code) |

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

- [Unsupported features](./systemverilog-reference#what-is-not-supported)
- [Partially supported features](./systemverilog-reference#partial-support)
- [Narrow limitations](./systemverilog-reference#narrow-limitations)
- [Deviations affecting standard-conforming code](./systemverilog-reference#deviations-standard-code)
- [Permissive acceptance of non-standard code](./systemverilog-reference#deviations-nonstandard-code)
- [Event scheduling choices](./systemverilog-reference#timing-semantics)
- [Time units and precision defaults](./systemverilog-reference#time-and-precision)
- [Random seeds and reproducibility](./systemverilog-reference#random-numbers)
- [Termination and `final` procedures](./systemverilog-reference#celox-execution-model)
- [Memory files (`$readmemh` / `$readmemb`)](./systemverilog-reference#memory-files)

## SystemVerilog emitted by Veryl

The Veryl conformance suite also checks the SystemVerilog emitted by Veryl.
Some cases cannot be emitted or have different language semantics; see the
[remaining exclusions](./systemverilog-reference#systemverilog-emitted-by-veryl).
