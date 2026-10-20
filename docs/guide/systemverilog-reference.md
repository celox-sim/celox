---
outline: [2, 3]
---

# SystemVerilog Reference

For setup and a first simulation, start with [SystemVerilog Support](./systemverilog).
This page describes individual constructs, restrictions and runtime behavior.

## Supported constructs {#what-is-supported}

The subset is the part of SystemVerilog that describes synchronous RTL for
synthesis and is tested at the design level.

### Structure and connections

| Area | Supported |
| --- | --- |
| Modules | ANSI ports, `parameter` / `localparam`, type parameters (`parameter type`), named and positional port and parameter connections, hierarchy, instance arrays (a connection is broadcast, or split between the elements when it is as wide as all of them) |
| Generate | `for` (with `genvar`), `if`, `case`; constant expressions including `**` |
| Packages | types, parameters, enums and functions, through `import p::*;`, `import p::x;` and `p::x` |
| Interfaces | interfaces with value and type parameters, header package imports, `localparam`s, `typedef`s, variables and nets, logic (`assign`, `always_comb`, `always_ff`, generate) and functions; interface instances and instance arrays, also inside generate blocks and other interfaces; interface ports with or without a modport, generic `interface` and `interface.mp` ports, and arrays of interface ports; explicit and implicit named interface connections (`.bus(bus)` and `.bus`); modport `input` / `output` members and imported functions; access to members, parameters and functions through an instance or port (`h[i].m`, `h.P`, `h.f(...)`, `h[i].f(...)` with a constant element index) |

### Data and expressions

| Area | Supported |
| --- | --- |
| Types | `logic`, `bit`, `reg`, packed vectors, multidimensional packed arrays with ascending or descending ranges and arbitrary constant bounds, packed structs and hard packed untagged unions, enums (with implicit values), `typedef`, unpacked arrays used as memories, signed and unsigned |
| Expressions | arithmetic including `**`, logic, shift, comparison, reduction, concatenation and replication, `?:`, `inside`, `==?` / `!=?`, casts (`N'(x)`, `signed'(x)`, `T'(x)`), `$signed` / `$unsigned` |
| Selects | constant and run-time bit selects and indexed part-selects (`[i]`, `[i +: W]`, `[i -: W]`), in reads and writes, in either declaration direction |
| Patterns | assignment patterns for packed structs, packed arrays and unpacked arrays (`'{a, b}`, `'{x: a, default: 0}`, `'{n{a}}`, `T'{...}`) |
| Parameters | integral parameters and overrides preserving widths beyond 128 bits and X/Z values, and parameters of unpacked array or packed struct type given by an assignment pattern (constant tables), including unpacked-array pattern overrides |

### Processes and control flow

| Area | Supported |
| --- | --- |
| Continuous logic | `assign`, `wire w = expr;` |
| Combinational processes | `always_comb`, `always @*`, block-local variables, sequential and dependent blocking assignments, reads of a variable before the process writes it (its previous value) |
| Sequential processes | `always_ff @(posedge clk)`, `always @(posedge clk or negedge rst_n)` and the like, blocking and nonblocking assignments, concatenated targets, four-state clock and reset signals, one asynchronous reset shared by several clock domains, processes at both edges of one clock or reset, and one signal as the clock of some processes and the asynchronous reset of others (a timed `Simulation` runs every edge; `Simulator::tick` is the rising edge of its event only) |
| Initial blocks | `initial` blocks and variable declaration initializers (`logic [7:0] v = 8'h5a;`, also of interface members); initializers take effect before the `initial` blocks. One whose writes have constant values, with constant `if` / `for` and `$readmemh` / `$readmemb`, defines the initial state. Any other, such as one that reads design state or calls a system task, runs as a process from time zero in a timed `Simulation`; a `Simulator` does not run processes |
| Statements | `if` / `else`, `case`, `casez`, `casex`, `case ... inside`, `unique` / `priority`, `for`, `while`, `do ... while`, `repeat`, `forever` and `foreach` (unrolled when the trip count is constant, otherwise executed at run time), `break` / `continue` / `return`, immediate assertions |
| Functions | `function` and `task` with `input`, `output` and `inout` arguments, `return` or assignment to the function name, local variables and `localparam`s, selected and composite assignments; calls are inlined. Calls with constant arguments in constant expressions (parameters, ranges) are evaluated during elaboration |

### Timed task calls

An `initial` or ordinary `always` process can call tasks containing `#delay`,
`@(event)` and `wait (condition)`, including through nested tasks. Calls are
inlined into the caller's resumable state machine. A normal edge-sensitive
`always` that calls a timed task waits for that task to return before rearming
its event control. Functions, `always_comb`, `always @*` and `always_ff` cannot
call a timed task.

Automatic task arguments and locals have separate storage for each activation
and survive suspension. Module-local static task arguments remain shared. Inputs and inouts
are copied in at entry; outputs and inouts are copied back when the task returns
(IEEE 1800-2023 13.3–13.3.2). Timed tasks with static local declarations remain
unsupported; use automatic locals. Package and `$unit` timed tasks with static
arguments, and `wait` conditions reading static task arguments, are rejected
until shared argument storage and its change notifications are supported.
Event expressions have the same restrictions
as waits written directly in a process.

### Built-in functions and tasks

| Area | Supported |
| --- | --- |
| System functions | `$timeunit`, `$timeprecision`, `$bits`, `$size`, `$left`, `$right`, `$low`, `$high`, `$increment`, `$dimensions`, `$unpacked_dimensions`, `$clog2`, `$countbits`, `$countones`, `$onehot`, `$onehot0`, `$isunknown`, `$isunbounded` in expressions, constant expressions and as statements; `$signed`, `$unsigned` in expressions, constant expressions and as statements |
| Random numbers | `$urandom`, `$urandom_range`, `$random` and `$dist_uniform` in clocked processes, `always_comb`, `always @*`, `initial` blocks and their subroutines; an optional integral seed of `$urandom`, a seed variable of `$random`, and a separate stream per process |
| Simulation time | `$time` and `$stime` in clocked and combinational processes, continuous assignments, `initial` blocks and their subroutines |
| System tasks | `$printtimescale`, `$display`, `$write` and their `b` / `o` / `h` forms, `$error`, `$warning`, `$info`, `$fatal`, `$finish`, `$stop`, `$exit` in `always` processes, subroutines and `initial` blocks; `$readmemh` / `$readmemb` there and in `initial` blocks; Veryl's `$assert` and `$assert_continue` |
| Command-line arguments | `$test$plusargs` and `$value$plusargs` in timed `initial` blocks and their subroutines |
| State | two-state and four-state simulation |

Every construct above is covered by tests that compare the result with a
software model. The shared Veryl conformance suite also runs against the
SystemVerilog emitted by Veryl.

## Unsupported constructs {#what-is-not-supported}

Celox reports an `Unsupported` error naming the construct instead of ignoring
it. The error carries the number of the issue that tracks the construct;
constructs without a dedicated issue point to the frontend roadmap, [#88](https://github.com/celox-sim/celox/issues/88).

- Classes. Timed tasks with static local declarations, recursive calls and `ref` arguments remain unsupported.
- Behavioral and verification constructs: `final`, intra-assignment timing controls,
  delayed continuous assignments ([#444](https://github.com/celox-sim/celox/issues/444)),
  named events, `fork` / `join`, concurrent assertions, `force` / `release`.
- `always_latch` ([#431](https://github.com/celox-sim/celox/issues/431)) and incomplete combinational assignments that would infer a latch.
- Ports and instances: non-ANSI port declarations ([#426](https://github.com/celox-sim/celox/issues/426)), `ref` ports
  ([#427](https://github.com/celox-sim/celox/issues/427)), wildcard `.*` connections ([#442](https://github.com/celox-sim/celox/issues/442)), gate primitives ([#457](https://github.com/celox-sim/celox/issues/457)),
  `bind`.
- Declarations: variable declaration initializers of members of an interface
  array, ANSI port default values, tagged unions, soft packed unions, packed union assignment patterns, internal nets without a driver ([#460](https://github.com/celox-sim/celox/issues/460)),
  unpacked structs, strings and `real`.
- Sequential processes: `iff` qualifiers ([#452](https://github.com/celox-sim/celox/issues/452)) and edge operands other than a
  plain signal ([#464](https://github.com/celox-sim/celox/issues/464)) in event lists.
- Combinational processes: nonblocking assignments inside `always_comb`
  ([#453](https://github.com/celox-sim/celox/issues/453)).
- Loops: a procedural loop unrolls at most 10,000 iterations; a combinational
  loop with a run-time trip count must be a counted `for` loop. Loop-generate
  constructs that run past 10,000 iterations are rejected ([#448](https://github.com/celox-sim/celox/issues/448)), and genvar
  updates with bitwise compound operators are rejected ([#455](https://github.com/celox-sim/celox/issues/455)).
- Expressions: streaming concatenations ([#447](https://github.com/celox-sim/celox/issues/447)), reduction operators in parameter expressions
  ([#456](https://github.com/celox-sim/celox/issues/456)).
- System tasks and functions: Celox knows every name IEEE 1800-2023
  defines, including the optional ones of its Annex D. One it does not support where it is called, such as
  `$realtime` in an expression or `$fopen`, is
  reported with its name wherever it is written, including in an unused
  parameter and in a function body. A `$` name that is not a system task or
  function, a call with the wrong number of arguments, an omitted argument
  without a declared default, and a task used as a value are errors.
- DPI-C beyond the subset described in [DPI-C imports](#dpi-c-imports), DPI
  exports, tri-state buses and multiple drivers, hierarchical references.

### Interface restrictions {#interface-restrictions}

Interfaces are supported within the limits below:

- **Declarations:** ports on an interface, tasks, implicit nets,
  enums and module instances inside an interface, and virtual interfaces are unsupported.
  Macros and compiler directives inside an interface are also unsupported.
- **Modports and connections:** `inout`, `ref`, expression ports, `export` and
  clocking modports are unsupported. A connection cannot name a modport if the
  receiving port declares none. Writes through a port without a modport are
  unsupported, as are member connections from such a port to a non-ANSI module.
- **Function calls:** calls through an unselected interface array or a run-time
  element selection are unsupported, as are calls from an interface instance array's logic to a function that accesses
  members. Calls through a port to an imported function that writes members are
  unsupported inside generate constructs, functions and tasks.
- **Declaration dependencies:** member references in parameter, constant, type
  or member declarations of an interface used through a port are unsupported.
  So are compilation-unit items used by an interface when a module in another
  file, or before the interface, uses it.
- **Names and hierarchy:** references to implicit generate names (`genblk1`) and
  paths through generate blocks (`g.h.x`) are unsupported. Declarations cannot
  reuse an interface instance or port name. Package names brought into a module
  by its interfaces cannot collide with another imported package's item or a
  module declaration.
- **Identifiers:** designs with interfaces cannot use `$` in their own names,
  or escaped identifiers that are not simple identifiers. Expansion reserves
  `$` for generated names.

## Runtime behavior {#semantics-worth-knowing}

### Instance array elements

Elements are addressed by their declared
index: `child_signal(&[("u", 3)], "y")` and `dut.u[3]` reach `u[3]` of
`Child u[3:2](...)`, and `InstanceHierarchy::index` holds the index. Arrays
with a negative bound are rejected.

### Out-of-range selects

A run-time select that reaches past either end of a
vector reads `X` for the missing bits (zero in two-state simulation) and
writes only the bits that exist.

### Wildcard comparisons

`casez`, `casex`, `inside` and `==?` honor the
`?`, `x` and `z` bits of a constant pattern in both two-state and four-state
simulation.

### Packages

Packages are analyzed once each, as scopes of their own, and names
resolve as IEEE 1800-2023 26.3 describes. `p::x` names the item of package
`p`, so items of one name in several packages, or in a package and a
module, do not clash. `import p::x;` makes only `x` visible. With
`import p::*;`, a name the scope declares itself hides the package's, and
a name that two wildcard-imported packages declare is an error only when a
reference uses it. Names in a package function resolve in the package.
An import in a generate block makes names visible in that block (and the
blocks inside it), ahead of the module's own declarations and imports.
A package variable is one object shared by every module that uses it,
including SystemVerilog modules instantiated from Veryl, and keeps its
initializer. `sim.signal("p::v")` names it. Several drivers of one package
variable are rejected, counting every instance of a module that writes it.
A package variable cannot be a clock or reset yet, and its initializer must
be constant. `export` declarations (26.6) make imported declarations
visible to the importers of a package: `export p::*;` exports only the
names the package actually imports from `p`, and an import through an
export denotes the original declaration, so importing it along two paths
is not ambiguous; `q::x` names the declaration `q` exports as `x`. Package
nets are not supported ([#1146](https://github.com/celox-sim/celox/issues/1146)).

### Compilation units

Each source file is a compilation unit of its own
(IEEE 1800-2023 3.12.1). Its parameters, types, subroutines, variables and
imports outside any module or package form the scope `$unit`, which
encloses the modules of the file: a module sees the unit's items and
imports declared before it, and every subroutine of the unit, after its own
declarations and imports. `$unit::x` names the unit's item `x`. A
compilation-unit variable is one object shared by the modules of the file,
named `$unit::v` like a package variable; only one source file may declare
them. DPI imports at compilation-unit scope are not supported.

### Interfaces

Interfaces are expanded into the modules that use them before
analysis. The members of an interface instance `h` become signals `h$m` of
the module that instantiates it, and its logic runs in that module. An
interface port `p` becomes one port `p$m` per member its modport lists, with
the modport direction, and its parameters become parameters `p$P`; a port
without a modport has every member, as an output when the module or a child
writes it. A top module with an interface port therefore has ports such as
`bus$data`. A module with a generic `interface` port is copied once per
interface it is connected to, as `M$I`.

Interface type parameters are specialized per instance and forwarded through
modport ports. Nested interface instances retain their own members and
parameters; recursive specializations and nesting beyond 64 levels are rejected.

### Block-local variables

Block-local variables have lexical scope in `always_comb` and `always_ff`.
Locals may shadow module signals and reuse names in separate blocks or processes.

### Hard packed untagged unions

Hard packed untagged unions share one bit vector between all members;
every member must have the same width (IEEE 1800-2023 7.3.1).

### Named subroutine arguments

Named subroutine arguments bind by formal name. Input evaluation and output
copy-out retain source order. Expressions and call statements support omitted
arguments with ANSI-declared defaults. Defaults are evaluated only when used,
in the enclosing declaration scope (IEEE 1800-2023 13.5.3). Constant calls use
the same formal names in parameter initializers, ranges and generate conditions;
generate-local declarations retain their own signatures. Named calls with
procedural effects in selection indices are rejected.

### Typed parameter patterns

Typed parameter patterns are converted in the caller's type, preserving
width, signing and four-state values (IEEE 1800-2023 10.9, 6.20.2). Packed
values transfer by bits, so struct member names and field divisions may differ
in the child. Typed scalar patterns can override a parameter whose default is
an ordinary value. Unpacked patterns retain their positional array shape.

### Variable lifetimes

Variable lifetimes follow IEEE 1800-2023 6.21. A variable declared in a
procedural block, function or task is static unless it, its subroutine or
its module (`module automatic`) is declared `automatic`. A static local of a
block keeps its value from one execution of the block to the next, and its
initializer runs once at time zero: in `always_ff`, `int count; count++;`
counts. An automatic local is initialized on every entry. A static local
whose value on entry is never read, because every path writes it before
reading it, behaves the same either way. Because a static local keeps its
value, a local of `always_comb` that only some paths write is a latch, and
is rejected like any other latch. A static local of a function or task
that keeps its value between calls is not supported, nor is a static local
whose initializer is not constant; declare them `automatic`. The result of
a static function is static too, so one with a path that does not assign
it (other than a `case` whose items cover every value of a two-state
argument) is
rejected as well. An interface
whose default lifetime (`interface automatic`) differs from that of the
module it is used in is rejected.

### Run-time loops

A loop in `always_ff` whose iteration count depends on
run-time values runs as a loop in the generated code; one that stops making
progress reports a runtime error naming its loop variable.

### Unbounded parameters

`$isunbounded(P)` returns a one-bit unsigned result: 1 for a value parameter assigned `$`, otherwise 0. The symbolic value survives parameter aliases and instance overrides (IEEE 1800-2023 6.20.7, 20.6.3).

### System functions called as statements

System functions called as statements, such as `$countones(f(a));`, are
checked and evaluated like the same call in an expression, and their value
is discarded. `$bits` and `$size` do not evaluate their operand.

### Simulation suspension

`$stop` in an `initial` block suspends the
simulation (IEEE 1800-2023 20.2): `Simulation::step` and `run_until` return
at the time of the stop with `is_stopped()` set, the other processes of that
time wait, and the next call continues after the `$stop`, also inside loops
and functions. The suspended time step keeps the edges the process made
before the stop. A checkpoint or state file cannot be taken while the
simulation is suspended (`CheckpointError::Stopped`). In an `always` process, `$stop` is reported as
`RuntimeEvent::Stop`, as `$finish` is reported as `RuntimeEvent::Finish`.
Its diagnostic level argument prints nothing. Batch hosts without an
interactive mode, such as the VPI host, end the run at `$stop`.

### `$exit`

`$exit` ends the initial procedures of the program block that calls it
(24.7). Celox has no program blocks, and a call from anywhere else is
ignored, as 24.7 specifies, with a compilation warning
(`Simulator::warnings()`) at the call. This includes a call in a constant
function. Calls in function declarations are diagnosed even when the function
is unused; the diagnostic does not depend on constant evaluation executing it.

### Immediate cover

Immediate cover executes its pass statement when the condition is true.
Coverage counters and reports are not exposed. Deferred cover (`#0` / `final`)
is rejected because its scheduling is not modeled.

### Constant functions

Constant functions ignore the display and severity tasks they call.

### `$readmemh` / `$readmemb`

`$readmemh` / `$readmemb` read their file when the design is compiled. A
missing or malformed file is a `MemoryFile` error. In `always_ff` the file's
words are written on every activation; in `initial` they are part of the
initial state. Explicit ranges load in either direction, and `@` file addresses
are absolute array indices. An address directive outside the requested range
is an error. Without directives, a word-count mismatch is a compilation
warning (`Simulator::warnings()`): excess words are ignored, and missing words
leave memory unchanged.

### Constant parameters of aggregate type

Constant parameters of aggregate type (unpacked arrays, packed structs
given by a pattern) are variables that hold their value from the start of
the simulation; they cannot be used where an elaboration-time constant is
required.

### Command-line arguments

Use `Simulation::from_sv_sources` for plusargs queries in `initial` blocks
and their subroutines. Queries from combinational and edge-triggered processes
are currently unsupported.

Supply ordered arguments with `.plusargs(["+COUNT=12", "+TRACE"])`.
`$test$plusargs` matches a prefix; `$value$plusargs` supports integer conversions
and `%s` into packed integral variables. The first matching argument wins, and
an unmatched value query preserves its destination. Real conversions and dynamic
strings are unsupported.

## Time and precision {#time-and-precision}

`$timeunit` and `$timeprecision` return a signed 32-bit exponent in seconds
(IEEE 1800-2023 20.4.1). They resolve the current time scope, `$unit`, `$root`,
and scalar module instance paths across source files. `timeunit`,
`timeprecision`, preceding `timescale` directives, and `resetall` follow the
precedence in 3.14.2.3. The implementation defaults are 1 ns / 1 ps. The global
precision includes all declared precisions and directive precisions, even in
uninstantiated definitions (3.14.3). Selected instance arrays and generate paths
require elaborated hierarchy lookup and are currently rejected.

`$printtimescale` displays the selected time scope in the IEEE 1800-2023
20.4.2 form, for example `Time scale of (Top.child) is 100ns / 10ps`.
It accepts the current scope, a design-element argument, `$unit`, or `$root`,
and runs in `initial`, `always` processes and subroutines. Current and relative
module names use the elaborated instance path. Package and interface copies
retain their declaration's time unit and precision.

Separate time unit and precision declarations may precede other scope items;
matching repeats may appear later. Designs mixing explicitly specified or
inherited scales with implementation defaults are rejected (3.14.2.3).
Module definitions and packages retain independent scales even when their names
coincide (3.13). A bare package query remains available when a same-named
module is instantiated; visible hierarchy instances, including roots, take
precedence over a namesake package. Simulator root lookup uses the selected
top and reachable hierarchy; instantiations in unused supplied modules do not
hide the selected root. Leading zeros in integer or fixed-point
time magnitudes do not change their numeric value. Interface functions retain
their declaration's time scope and compilation unit when copied into modules
during elaboration. Queries of scalar interface instances and ports are
resolved using their bound interface type before flattening, including generic
interface ports and handles declared within a generate block. Scalar paths to
interface instances inside child modules, such as `child.info`, are resolved
before those instances are flattened away.


A `Simulation` counts time in the design's global time precision (IEEE
1800-2023 3.14.3), 1 ps by default: `run_until(1000)` runs to 1 ns, and `#10`
in a module whose time unit is 1 ns waits 10,000 ps (3.14.1). `$time`
returns the time in the time unit of the scope that calls it, rounded to an
integer, halves up (20.3.1); a package function uses its package's unit.
`$stime` returns the low 32 bits of that value (20.3.2). Combinational logic
reads the time when its inputs change, not when only the time does. A
`Simulator`, which has no time, returns 0. `$realtime` waits for `real`
support.

## Random numbers {#random-numbers}

`$urandom` uses a deterministic, full-period 32-bit linear congruential generator
(IEEE 1800-2023 18.13.1 and 18.14). Reseeding repeats the sequence without
modifying the seed argument. Calls through subroutines use their caller's
stream. The generator state participates in checkpoints and state restoration.
The default seed is implementation-defined; random values need not match those
of other simulators. In combinational procedures, draws occur when the process
activates: `always_comb` runs once at time zero and on implicit sensitivity
changes, while `always @*` waits for a sensitivity change. Repeated evaluation
with unchanged inputs does not advance the stream. Constant-value contexts
report a diagnostic.

`$urandom_range(maxval, minval)` draws from the `$urandom` stream and returns a
value between the bounds, inclusive, swapping reversed bounds; `minval`
defaults to 0 (18.13.2). `$random` follows the algorithm of IEEE 1800-2023
Annex N, so it returns the same values as other simulators that implement it,
such as Icarus Verilog. `$random(seed)` updates its seed variable. Without an
argument, each process continues its own seed, which starts at 0, so a design
whose one process calls `$random` matches simulators that keep one seed for
the whole design.

`$dist_uniform(seed, start, end)` uses the Annex N uniform distribution, updates
its integral seed variable and includes both bounds. Equal or reversed bounds
return `start` without advancing the seed.

## Mixing Veryl and SystemVerilog {#mixed-language}

`Simulator::from_mixed_sources`combines both source lists in one simulation.
Veryl instantiates an SV module with `$sv::Module`. SV refers to a Veryl module
or interface by the name the Veryl emitter gives it, as SV written against the
emitted code does: `prj_Module` for module `Module` of project `prj`, or
`Module` when `omit_project_prefix` is set in `Veryl.toml`. Without a
`Veryl.toml` the project is named `prj`. An SV module whose name equals the
emitted name of a Veryl module is a duplicate definition. A Veryl top is named
by its Veryl name. Numeric module parameters are resolved by the owning
frontend, including parameters of Veryl children instantiated from SV.

Interfaces and `input` / `output` modports may cross the boundary in either
direction. In Veryl, use `inst bus: $sv::Bus;` and
`bus: modport $sv::Bus::slave` for an SV interface. SV can instantiate an
interface declared in Veryl and connect it to either language's modules.
Interface parameters, instance arrays and generic SV interface ports are
bound to the actual interface. Logic inside an SV interface instantiated
from Veryl runs in the SV frontend.

Declared parameter widths retain dependencies on earlier interface parameters,
including when SV supplies named overrides in a different order.
Constant functions used by interface defaults are expanded into declaration
expressions when the native analyzer can represent their bodies that way.
Header-dependent functions with statement bodies that cannot be expressed in
the contract, such as loops, produce an unsupported diagnostic. Functions with
header-dependent formal or return widths also produce an unsupported diagnostic;
the contract cannot represent those symbolic type conversions yet. Declaration
casts whose target size depends on interface parameters and constant functions
that cannot be lowered symbolically are also rejected instead of retaining a
value computed from default parameters.

The bridge exchanges declaration contracts and links native symbolic modules;
it does not require the Veryl-to-SV emitter. Cross-language interface function
calls and function `import` / `export` modports are not supported yet. They
remain available within their owning language's supported subset.

## DPI-C imports {#dpi-c-imports}

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

## SystemVerilog emitted by Veryl {#systemverilog-emitted-by-veryl}

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
