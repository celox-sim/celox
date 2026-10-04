# SystemVerilog Support

Celox simulates [Veryl](https://veryl-lang.org/) and a **synthesizable subset of
SystemVerilog**. Both frontends lower into the same intermediate representation,
so they share scheduling, optimization, backends, and the runtime. A design can
also mix the two languages: a Veryl top level may instantiate SystemVerilog
modules.

The SystemVerilog frontend is a Rust API behind the `systemverilog` Cargo
feature of the `celox` crate. The TypeScript package and the Vite plugin
currently load Veryl only.

```toml
[dependencies]
celox = { version = "*", features = ["systemverilog"] }
```

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
```

`Simulator::from_mixed_sources` combines Veryl sources with SystemVerilog
children.

## What is supported

The subset is the part of SystemVerilog that describes synchronous RTL for
synthesis and is tested at the design level.

| Area | Supported |
| --- | --- |
| Modules | ANSI ports, `parameter` / `localparam`, named and positional port and parameter connections, hierarchy |
| Generate | `for` (with `genvar`), `if`, `case`; constant expressions including `**` |
| Types | `logic`, `bit`, `reg`, packed vectors, packed arrays, packed structs, enums (with implicit values), `typedef`, unpacked arrays used as memories, signed and unsigned |
| Packages | types, parameters, enums and functions, through `import p::*;`, `import p::x;` and `p::x` |
| Continuous logic | `assign`, `wire w = expr;` |
| Combinational processes | `always_comb`, `always @*`, block-local variables, sequential and dependent blocking assignments |
| Sequential processes | `always_ff @(posedge clk)`, `always @(posedge clk or negedge rst_n)` and the like |
| Statements | `if` / `else`, `case`, `casez`, `casex`, `unique` / `priority`, `for` with constant bounds |
| Functions | `function` with `return` or assignment to the function name; calls are inlined |
| Expressions | arithmetic, logic, shift, comparison, reduction, concatenation and replication, `?:`, `inside`, `==?` / `!=?`, casts (`N'(x)`, `signed'(x)`, `T'(x)`), `$signed` / `$unsigned` |
| Selects | constant and run-time bit selects and indexed part-selects (`[i]`, `[i +: W]`, `[i -: W]`), in reads and writes, in either declaration direction |
| Patterns | assignment patterns for packed structs (`'{a, b}`, `'{x: a, default: 0}`) |
| System functions | `$bits`, `$size`, `$clog2` (constant argument), `$countones`, `$onehot`, `$onehot0`, `$isunknown` |
| State | two-state and four-state simulation |

Every construct above is covered by tests that compare the result with a
software model. The shared Veryl conformance suite also runs against the
SystemVerilog emitted by Veryl.

## What is not supported

Celox reports an `Unsupported` error naming the construct instead of ignoring
it.

- Interfaces and modports, classes, tasks, and function `output` / `inout`
  arguments.
- Module instance arrays and type parameters (`parameter type`).
- Behavioral and verification constructs: `initial`, `final`, delays, event
  controls other than clock edges, assertions, `$display` and other system
  tasks, `force` / `release`.
- `always_latch`, level-sensitive sensitivity lists other than `@*`, and
  incomplete combinational assignments that would infer a latch.
- Loops whose bound is not constant (`while`, `repeat`, `forever`), `break` and
  `continue`.
- Exponentiation with a run-time operand.
- Unions, unpacked structs, strings, `real`, DPI, gate primitives, tri-state
  buses and multiple drivers, hierarchical references.
- Four-state clock or reset signals in `always_ff` event lists.

## Semantics worth knowing

- **Out-of-range selects.** A run-time select that reaches past either end of a
  vector reads zero for the missing bits and writes only the bits that exist.
  Four-state simulation does not yet turn a fully out-of-range read into `X`.
- **Wildcard comparisons.** `casez`, `casex`, `inside` and `==?` honor the
  `?`, `x` and `z` bits of a constant pattern in both two-state and four-state
  simulation.
- **Packages** are inlined into each module that uses them. Names resolve by
  their plain identifier, so a package item and a module item with the same
  name are reported as a duplicate declaration.
- **Block-local variables** of an `always_comb` become signals of the module; a
  name that clashes with another signal is rejected.
