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
| Interfaces | interfaces with parameters, `localparam`s, `typedef`s, variables and nets, logic (`assign`, `always_comb`, `always_ff`, generate) and functions; interface instances and instance arrays, also inside generate blocks; interface ports with or without a modport, generic `interface` and `interface.mp` ports, and arrays of interface ports; modport `input` / `output` members and imported functions; access to members, parameters and functions through an instance or port (`h[i].m`, `h.P`, `h.f(...)`) |
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
| System functions | `$bits`, `$size`, `$clog2`, `$countones`, `$onehot`, `$onehot0`, `$isunknown` in expressions, constant expressions and as statements; `$signed`, `$unsigned` in expressions and as statements |
| System tasks | `$display`, `$write` and their `b` / `o` / `h` forms, `$error`, `$warning`, `$info`, `$fatal`, `$finish`, `$stop` in `always` processes and subroutines; `$readmemh` / `$readmemb` there and in `initial` blocks; Veryl's `$assert` and `$assert_continue` |
| State | two-state and four-state simulation |

Every construct above is covered by tests that compare the result with a
software model. The shared Veryl conformance suite also runs against the
SystemVerilog emitted by Veryl.

## What is not supported

Celox reports an `Unsupported` error naming the construct instead of ignoring
it. The error carries the number of the issue that tracks the construct;
constructs without a dedicated issue point to the frontend roadmap, [#88](https://github.com/celox-sim/celox/issues/88).

- Classes, and tasks with timing controls.
- Interfaces: ports of an interface, `inout`, `ref` and expression modport
  ports, modport `export` and clocking, tasks, type parameters, implicit nets, enums and
  instances inside an interface, a modport named in a connection to a port
  that does not declare one, calls of functions of an interface array,
  writes through a port without a modport, and calls through a port of an
  imported function that writes members, inside a generate construct or a
  function or task, connections of a member of such a port to a module that
  is not declared with an ANSI header, calls from the logic of an interface instance array of a
  function that accesses members, member references in the parameter,
  constant, type or member declarations of an interface used through a port,
  macros and compiler directives in an interface, compilation-unit items
  that an interface uses when a module of another source file or before it
  uses the interface, references to implicit generate block names (`genblk1`) in an
  interface, hierarchical references to an interface instance through a
  generate block (`g.h.x`),
  declarations that reuse the name of an
  interface instance or port, package items of one name reaching a module
  from two packages through the imports of its interfaces (or hidden by a
  declaration of the module), and virtual interfaces. A design with
  interfaces may not use `$` in its own identifiers, which the expansion
  reserves for generated names, or escaped identifiers that are not simple
  identifiers.
- Behavioral and verification constructs: `initial` blocks that read design
  state or use timing, `final`, delays and delayed continuous assignments
  ([#444](https://github.com/celox-sim/celox/issues/444)), event controls other than clock edges, concurrent assertions,
  `force` / `release`.
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
- System tasks and functions: Celox knows every name of IEEE 1800-2023
  clauses 20 and 21. One it does not support where it is called, such as
  `$time` in an expression, `$fopen`, or `$display` in an `initial` block, is
  reported with its name wherever it is written, including in an unused
  parameter and in a function body. A `$` name that is not a system task or
  function, a call with the wrong number of arguments or an omitted argument
  of a function, and a task used as a value are errors.
- DPI-C beyond the subset described in [DPI-C imports](#dpi-c-imports), DPI
  exports, tri-state buses and multiple drivers, hierarchical references.

## DPI-C imports

The `sv-dpi` Cargo feature (which implies `systemverilog`) lets a design call C
functions declared with `import "DPI-C"`. Without it, Celox rejects any DPI-C
import as unsupported.

```toml
[dependencies]
celox = { version = "*", features = ["sv-dpi"] }
```

```systemverilog
module Top(input logic clk, input int a, input int b, output int sum);
    import "DPI-C" function int c_add(input int x, input int y);
    always_ff @(posedge clk) sum <= c_add(a, b);
endmodule
```

Each import is linked by its C name (the name after `=` in
`import "DPI-C" c_name = function ...`, otherwise the SystemVerilog name) when
the simulator is built. Functions registered with `dpi_function` are searched
first, then the shared libraries added with `dpi_library`, in order. A name
that none of them defines fails the build with a `Dpi` error.

```rust
extern "C" fn c_add(x: i32, y: i32) -> i32 { x.wrapping_add(y) }

// SAFETY: `c_add` and the functions of libmodel.so have the signatures
// their imports declare.
let mut sim = unsafe {
    Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .dpi_function("c_add", c_add as *const ())
        .dpi_library("libmodel.so")
}
.build()?;
```

Both methods are `unsafe`: Celox cannot check that a C function matches the
prototype of its import, and loading a library runs its initializers.

Every backend calls the functions directly through the platform C ABI. The
first release supports this subset:

- Calls in `always_ff` processes, including in functions they call. Calls are
  made in statement order, once per execution of the statement. Calls in
  combinational logic are rejected.
- `function` imports (`pure` or not) declared in a module or in a package
  (reached through `import p::*;` or `p::f`), returning `void` or one of the
  argument types, with up to 16 `input` arguments. Imports linked to one C
  name must declare the same prototype.
- Argument and result types `bit`, `logic` (passed as `svBit` / `svLogic`),
  `byte`, `shortint`, `int` and `longint`, signed or `unsigned`.

Imports at compilation-unit scope, `output` and `inout` arguments, `context`
imports, task imports, exports, packed vector arguments (`svBitVecVal` /
`svLogicVecVal`), `integer`, `real`, `chandle` and `string` are rejected.

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
- **Interfaces** are expanded into the modules that use them before
  analysis. The members of an interface instance `h` become signals `h$m` of
  the module that instantiates it, and its logic runs in that module. An
  interface port `p` becomes one port `p$m` per member its modport lists, with
  the modport direction, and its parameters become parameters `p$P`; a port
  without a modport has every member, as an output when the module or a child
  writes it. A top module with an interface port therefore has ports such as
  `bus$data`. A module with a generic `interface` port is copied once per
  interface it is connected to, as `M$I`.
- **Block-local variables** of an `always_comb` become signals of the module; a
  name that clashes with another signal is rejected.
- **Run-time loops.** A loop in `always_ff` whose iteration count depends on
  run-time values runs as a loop in the generated code; one that stops making
  progress reports a runtime error naming its loop variable.
- **System functions called as statements**, such as `$countones(f(a));`, are
  checked and evaluated like the same call in an expression, and their value
  is discarded. `$bits` and `$size` do not evaluate their operand.
- **Constant functions** ignore the display and severity tasks they call.
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
  hierarchical assignments) are not emitted as simulatable SystemVerilog
  modules.
