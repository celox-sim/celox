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
| Modules | ANSI ports, `parameter` / `localparam`, type parameters (`parameter type`), named and positional port and parameter connections, hierarchy, instance arrays (a connection is broadcast, or split between the elements when it is as wide as all of them) |
| Generate | `for` (with `genvar`), `if`, `case`; constant expressions including `**` |
| Types | `logic`, `bit`, `reg`, packed vectors, packed arrays, packed structs, enums (with implicit values), `typedef`, unpacked arrays used as memories, signed and unsigned |
| Packages | types, parameters, enums and functions, through `import p::*;`, `import p::x;` and `p::x` |
| Continuous logic | `assign`, `wire w = expr;` |
| Combinational processes | `always_comb`, `always @*`, block-local variables, sequential and dependent blocking assignments, reads of a variable before the process writes it (its previous value) |
| Sequential processes | `always_ff @(posedge clk)`, `always @(posedge clk or negedge rst_n)` and the like, blocking and nonblocking assignments, concatenated targets, four-state clock and reset signals, one asynchronous reset shared by several clock domains |
| Initial blocks | `initial` blocks whose writes have constant values (they define the initial state), with constant `if` / `for`, and `$readmemh` / `$readmemb` |
| Statements | `if` / `else`, `case`, `casez`, `casex`, `case ... inside`, `unique` / `priority`, `for`, `while`, `do ... while`, `repeat`, `forever` and `foreach` (unrolled when the trip count is constant, otherwise executed at run time), `break` / `continue` / `return`, immediate assertions |
| Functions | `function` and `task` (without timing) with `input`, `output` and `inout` arguments, `return` or assignment to the function name, local variables and `localparam`s, selected and composite assignments; calls are inlined. Calls with constant arguments in constant expressions (parameters, ranges) are evaluated during elaboration |
| Expressions | arithmetic including `**`, logic, shift, comparison, reduction, concatenation and replication, `?:`, `inside`, `==?` / `!=?`, casts (`N'(x)`, `signed'(x)`, `T'(x)`), `$signed` / `$unsigned` |
| Selects | constant and run-time bit selects and indexed part-selects (`[i]`, `[i +: W]`, `[i -: W]`), in reads and writes, in either declaration direction |
| Patterns | assignment patterns for packed structs, packed arrays and unpacked arrays (`'{a, b}`, `'{x: a, default: 0}`, `'{n{a}}`, `T'{...}`) |
| Parameters | integral parameters, and parameters of unpacked array or packed struct type given by an assignment pattern (constant tables) |
| System functions | `$bits`, `$size`, `$clog2`, `$countones`, `$onehot`, `$onehot0`, `$isunknown` |
| System tasks | `$display`, `$write`, `$error`, `$warning`, `$info`, `$fatal`, `$finish` in `always_comb` and `always_ff`; Veryl's `$assert` and `$assert_continue` |
| State | two-state and four-state simulation |

Every construct above is covered by tests that compare the result with a
software model. The shared Veryl conformance suite also runs against the
SystemVerilog emitted by Veryl.

## What is not supported

Celox reports an `Unsupported` error naming the construct instead of ignoring
it. The error carries the number of the issue that tracks the construct;
constructs without a dedicated issue point to the frontend roadmap, [#88](https://github.com/celox-sim/celox/issues/88).

- Interfaces and modports, classes, and tasks with timing controls.
- Behavioral and verification constructs: `initial` blocks that read design
  state or use timing, `final`, delays and delayed continuous assignments
  ([#444](https://github.com/celox-sim/celox/issues/444)), event controls other than clock edges, concurrent assertions,
  `force` / `release`. System tasks inside a combinational loop whose trip
  count is only known at run time are rejected.
- `always_latch` ([#431](https://github.com/celox-sim/celox/issues/431)), level-sensitive sensitivity lists other than `@*`,
  and incomplete combinational assignments that would infer a latch.
- Ports and instances: non-ANSI port declarations ([#426](https://github.com/celox-sim/celox/issues/426)), `ref` ports
  ([#427](https://github.com/celox-sim/celox/issues/427)), wildcard `.*` connections ([#442](https://github.com/celox-sim/celox/issues/442)), gate primitives ([#457](https://github.com/celox-sim/celox/issues/457)),
  `bind`.
- Declarations: variable declaration initializers ([#439](https://github.com/celox-sim/celox/issues/439)), packed unions
  ([#440](https://github.com/celox-sim/celox/issues/440)), multidimensional packed ranges that are not zero-based and
  descending ([#438](https://github.com/celox-sim/celox/issues/438)), internal nets without a driver ([#460](https://github.com/celox-sim/celox/issues/460)), block-local
  variables that share a name with a variable of another process ([#445](https://github.com/celox-sim/celox/issues/445)),
  unpacked structs, strings and `real`.
- Sequential processes: `iff` qualifiers ([#452](https://github.com/celox-sim/celox/issues/452)) and edge operands other than a
  plain signal ([#464](https://github.com/celox-sim/celox/issues/464)) in event lists, and both edges of one clock ([#443](https://github.com/celox-sim/celox/issues/443)) or one
  reset ([#471](https://github.com/celox-sim/celox/issues/471)) across processes.
- Combinational processes: nonblocking assignments inside `always_comb`
  ([#453](https://github.com/celox-sim/celox/issues/453)).
- Loops: a procedural loop unrolls at most 10,000 iterations; a combinational
  loop with a run-time trip count must be a counted `for` loop. Loop-generate
  constructs that run past 10,000 iterations are rejected ([#448](https://github.com/celox-sim/celox/issues/448)), and genvar
  updates with bitwise compound operators are rejected ([#455](https://github.com/celox-sim/celox/issues/455)).
- Expressions: streaming concatenations ([#447](https://github.com/celox-sim/celox/issues/447)), reduction operators in parameter expressions
  ([#456](https://github.com/celox-sim/celox/issues/456)), and parameter overrides that are not plain integers, such as X/Z
  values or values wider than 128 bits ([#461](https://github.com/celox-sim/celox/issues/461)).
- DPI, tri-state buses and multiple drivers, hierarchical references.

## Semantics worth knowing

- **Instance array elements.** Elements are addressed by their declared
  index: `child_signal(&[("u", 3)], "y")` and `dut.u[3]` reach `u[3]` of
  `Child u[3:2](...)`, and `InstanceHierarchy::index` holds the index. Arrays
  with a negative bound are rejected.
- **Out-of-range selects.** A run-time select that reaches past either end of a
  vector reads `X` for the missing bits (zero in two-state simulation) and
  writes only the bits that exist.
- **Wildcard comparisons.** `casez`, `casex`, `inside` and `==?` honor the
  `?`, `x` and `z` bits of a constant pattern in both two-state and four-state
  simulation.
- **Packages** are inlined into each module that uses them. Names resolve by
  their plain identifier, so a package item and a module item with the same
  name are reported as a duplicate declaration.
- **Block-local variables** of an `always_comb` become signals of the module; a
  name that clashes with another signal is rejected.
- **Run-time loops.** A loop in `always_ff` whose iteration count depends on
  run-time values runs as a loop in the generated code; one that stops making
  progress reports a runtime error naming its loop variable.
- **`$readmemh` / `$readmemb`** read their file when the design is compiled. A
  missing or malformed file is a `MemoryFile` error. In `always_ff` the file's
  words are written on every activation; in `initial` they are part of the
  initial state.
- **Constant parameters of aggregate type** (unpacked arrays, packed structs
  given by a pattern) are variables that hold their value from the start of
  the simulation; they cannot be used where an elaboration-time constant is
  required.

## SystemVerilog emitted by Veryl

The Veryl conformance suite runs against the SystemVerilog that Veryl emits.
The cases that remain excluded fall into these groups:

- Veryl emits invalid SystemVerilog for an array literal that mixes positional
  items with `default:` (`'{a, default: b}`).
- Celox's Veryl frontend can opt into function calls with output arguments or
  side effects in `always_ff`, which Veryl rejects because the emitted
  SystemVerilog call would copy the outputs out immediately instead of with
  the nonblocking semantics of Veryl. These designs have no SystemVerilog
  equivalent and are not emitted.
- Testbench modules (`initial` blocks with clocking and `$finish` scheduling,
  hierarchical assignments) and interfaces are not emitted as simulatable
  SystemVerilog modules.
