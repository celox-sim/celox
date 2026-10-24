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
| Strings | `string` variables, function arguments and results: assignment of strings, string literals and casts (`string'(x)`), `==`, `!=` and ordering comparisons, concatenation and replication (with a run-time count), character reads and writes (`s[i]`), and the methods `len`, `getc`, `putc`, `toupper`, `tolower`, `compare`, `icompare`, `substr`, `atoi` / `atohex` / `atooct` / `atobin` and `itoa` / `hextoa` / `octtoa` / `bintoa`; `%s` and format-less display of string values. See [strings](#strings) |
| Classes | classes declared in a module: properties of 2-state integral types up to 64 bits, `string` and class handle types, with initializers; `new` with and without arguments, implicit and explicit constructors; methods (`function` and `task`, also `extern` with an out-of-block body) called through a handle or unqualified inside the class; `this`, `null`, handle assignment and comparison; inheritance of properties and methods, with base constructors called by `super.new(...)` or `extends Base(...)`; `super.` members; virtual methods, abstract (`virtual`) classes and pure virtual methods; classes that implement interface classes and method calls through interface class handles; `$cast` of class handles, as a function and as a task; static properties and methods; `C::` scope resolution of static members and `C::new`; arrays of handles. See [classes](#classes) |
| Parameters | integral parameters and overrides preserving widths beyond 128 bits and X/Z values, and parameters of unpacked array or packed struct type given by an assignment pattern (constant tables), including unpacked-array pattern overrides |

### Processes and control flow

| Area | Supported |
| --- | --- |
| Continuous logic | `assign`, `wire w = expr;` |
| Combinational processes | `always_comb`, `always @*`, blocking `always_latch`, block-local variables, sequential and dependent blocking assignments, reads of a variable before the process writes it (its previous value) |
| Sequential processes | `always_ff @(posedge clk)`, `always @(posedge clk or negedge rst_n)` and the like, blocking and nonblocking assignments, concatenated targets, four-state clock and reset signals, one asynchronous reset shared by several clock domains, processes at both edges of one clock or reset, and one signal as the clock of some processes and the asynchronous reset of others (a timed `Simulation` runs every edge; `Simulator::tick` is the rising edge of its event only) |
| Initial blocks | `initial` blocks and variable declaration initializers (`logic [7:0] v = 8'h5a;`, also of interface members); initializers take effect before the `initial` blocks. One whose writes have constant values, with constant `if` / `for` and `$readmemh` / `$readmemb`, defines the initial state. Any other, such as one that reads design state or calls a system task, runs as a process from time zero in a timed `Simulation`; a `Simulator` does not run processes, so building one for such a design fails with `SimulatorErrorKind::ProcessRequiresSimulation` |
| Statements | `if` / `else`, `case`, `casez`, `casex`, `case ... inside`, `unique` / `priority`, `for`, `while`, `do ... while`, `repeat`, `forever` and `foreach` (unrolled when the trip count is constant, otherwise executed at run time), `break` / `continue` / `return`, immediate assertions |
| Functions | `function` and `task` with `input`, `output` and `inout` arguments, `return` or assignment to the function name, local variables and `localparam`s, selected and composite assignments; calls are inlined. Calls with constant arguments in constant expressions (parameters, ranges) are evaluated during elaboration |

### Timing controls {#timing-controls}

In a timed `Simulation`, `initial` and plain `always` processes can suspend:

| Control | Supported |
| --- | --- |
| `#delay` | integral and real delays, `#(min:typ:max)` (the typical value) and [time literals](#time-literal-delays); an X or Z delay counts as zero |
| `@(...)` | variables, `posedge` / `negedge` / `edge`, items joined by `or` or `,`, bit, part and run-time index selects, any expression including calls of pure functions, and `@*` / `@(*)`, which waits on the variables its statement reads |
| `wait (condition)` | any condition; an X or Z condition counts as false |
| Assignments | blocking and nonblocking, to any target a blocking assignment takes, including unpacked array elements and run-time positions; nonblocking updates take effect in the NBA region of the time slot, at the position evaluated when the assignment executes, and bits outside the target or at a position with an X or Z bit are not written |

An `always` whose sensitivity list names exactly the variables its statement
reads, as `@*` would (IEEE 1800-2023 9.4.2.2), simulates like `always @*` in
both `Simulator` and `Simulation`: `always @(a or b) y = a + b;` or
`always @(b, a) y = a + b;`. The statement must not contain a timing control
or a nonblocking assignment. Any other sensitivity list without edges runs as
a process: `always @(a) y = a + b;` leaves `y` unchanged when only `b`
changes. So does a list that, as `always @*`, would close a combinational
loop with other logic. So does an edge-sensitive `always`
that does not have the shape of a flip-flop: one with an edge of a select or
an expression (`@(posedge v[0])`, `@(posedge (a & b))`), an `edge`, an item
without an edge next to edges, a multi-bit edge signal (its least significant
bit decides the edge), or several edges of which the body tests none as an
asynchronous reset. Only `always_ff` must have that shape. A `Simulator` runs
no processes, so building one for such a design fails with
`SimulatorErrorKind::ProcessRequiresSimulation`; use `Simulation`, or write
`always @*` or `always_comb`.

### Timed task calls

An `initial` or ordinary `always` process can call tasks containing `#delay`,
`@(event)` and `wait (condition)`, including through nested tasks. Calls are
inlined into the caller's resumable state machine. A normal edge-sensitive
`always` that calls a timed task waits for that task to return before rearming
its event control. Functions, `always_comb` and `always_ff` cannot call a timed
task, and `always @*` cannot yet ([#1710](https://github.com/celox-sim/celox/issues/1710)).

Automatic task arguments and locals have separate storage for each activation
and survive suspension. Module-local static task arguments remain shared. Inputs and inouts
are copied in at entry; outputs and inouts are copied back when the task returns
(IEEE 1800-2023 13.3–13.3.2). Timed tasks with static local declarations remain
unsupported; use automatic locals. Package and `$unit` timed tasks with static
arguments, and `wait` conditions reading static task arguments, are rejected
until shared argument storage and its change notifications are supported.
Event expressions have the same restrictions
as waits written directly in a process.

### Scope randomization

`randomize(a, b)` and `std::randomize(a, b)` support unconstrained whole packed
variables, including signed vectors and packed structures, at any supported
width. Each call fills every word with random zero/one bits and returns the
signed `int` success value `1`. An empty argument list succeeds without writing a
variable or advancing the random stream. Calls follow procedural statement order
and short-circuit evaluation, and use the calling process's persistent random
stream (IEEE 1800-2023 18.12).

Constraints (`with`), enum variables, unpacked arrays, nets,
constants, selects and concatenation arguments are unsupported and rejected.

### Class randomization

`object.randomize()` randomizes enabled `rand` properties and returns signed
`int` value `1`. Supported properties are 2-state integral values, including scalar enums, up
to 64 bits in classes without constraints. Enum randomization selects uniformly
from the declared values, including sparse and negative values (18.3, 18.4.1).
Enums with unresolved or X/Z values and ranged enumerator declarations remain
unsupported for randomization. Inherited properties are included,
and a call through a base handle uses the dynamic object's class and its
`pre_randomize()` / `post_randomize()` hooks. A derived hook replaces the base
hook; call `super.pre_randomize()` or `super.post_randomize()` explicitly to
include the base hook's work (IEEE 1800-2023 18.6.2). Hooks must be functions
without arguments, non-static and non-virtual. Calls inside a class may use
`randomize()` or `this.randomize()`.

`object.member.rand_mode(0)` disables a random property; `rand_mode(1)` enables
it, and `object.member.rand_mode()` returns its current mode as signed `int`.
The setter's argument is a `bit`. `object.rand_mode(bit)` sets all random
properties, including inherited ones. All start enabled, and aliases share the
same modes (18.8).

`object.randomize(x, y)` temporarily selects exactly those instance properties,
including supported properties not declared `rand`. Names are resolved in the
receiver's class, not the caller's scope; expressions and inaccessible members
are rejected. The temporary modes are installed before the pre-hook and restored
after the post-hook; hooks can inspect and change them. `randomize(null)` runs
the hooks without drawing or assigning random values (18.11). Constraints are
not supported even for this checker form.

Each object has an independent random stream, seeded from its creating process.
`object.srandom(seed)` resets it using a 32-bit seed; checkpoints preserve both
RNG and mode state (18.13.3, 18.14.3). Random sequences are implementation-specific.
`randc` randomization, constraints, `with`, static random properties, and random
array/object/string properties remain unsupported. A constrained dynamic
subclass is rejected rather than silently randomizing only its base properties.
A call through `null` is a runtime error.

### Time literal delays {#time-literal-delays}

Standalone delay controls accept time literals such as `#5ns`, `#(2.75ns)` and
`#40ps`, with all six units from `fs` to `s`. Values round at the declaring
scope's precision before conversion to scheduler ticks (5.8, 3.14.1). A real
delay such as `#1.5` or `#r` rounds the same way, with ties away from zero
(6.12.1); a negative one waits its two's complement (9.4.1), and a NaN one
waits zero.

### Built-in functions and tasks

| Area | Supported |
| --- | --- |
| System functions | `$timeunit`, `$timeprecision`, `$bits`, `$size`, `$left`, `$right`, `$low`, `$high`, `$increment`, `$dimensions`, `$unpacked_dimensions`, `$clog2`, `$countbits`, `$countones`, `$onehot`, `$onehot0`, `$isunknown`, `$isunbounded` in expressions, constant expressions and as statements; `$signed`, `$unsigned` in expressions, constant expressions and as statements |
| Random numbers | `$urandom`, `$urandom_range`, `$random` and `$dist_uniform` in clocked processes, `always_comb`, `always @*`, `initial` blocks and their subroutines; an optional integral seed of `$urandom`, a seed variable of `$random`, and a separate stream per process |
| Simulation time | `$time`, `$stime` and `$realtime` in clocked and combinational processes, continuous assignments, `initial` blocks and their subroutines |
| System tasks | `$printtimescale`, `$display`, `$write` and their `b` / `o` / `h` forms, `$error`, `$warning`, `$info`, `$fatal`, `$finish`, `$stop`, `$exit` in `always` processes, subroutines and `initial` blocks; `$readmemh` / `$readmemb` there and in `initial` blocks; Veryl's `$assert` and `$assert_continue` |
| Runtime I/O | `$test$plusargs`, `$value$plusargs`, `$fopen`, `$fclose`, `$fdisplay` / `$fwrite` and radix variants, `$writememh` / `$writememb`, and [VCD controls](./vcd#systemverilog-source-controls) in timed processes |
| State | two-state and four-state simulation |

Every construct above is covered by tests that compare the result with a
software model. The shared Veryl conformance suite also runs against the
SystemVerilog emitted by Veryl.

## Unsupported constructs {#what-is-not-supported}

Celox reports an `Unsupported` error naming the construct instead of ignoring
it. The error carries the number of the issue that tracks the construct;
constructs without a dedicated issue point to the frontend roadmap, [#88](https://github.com/celox-sim/celox/issues/88).

- Class features beyond those [listed](#classes): `$cast` to a variable that is not a class handle, typedefs and parameters of interface classes, classes declared in packages, parameterized classes, named constructor arguments, shallow copies (`new h`), and properties that are 4-state, `real`, arrays or wider than 64 bits. Timed tasks with static local declarations, recursive calls and `ref` arguments remain unsupported.
- Behavioral and verification constructs: `final`, intra-assignment timing controls (`a = #1 b`),
  delayed continuous assignments ([#444](https://github.com/celox-sim/celox/issues/444)),
  named events and `->`, `fork` / `join`, `wait fork` / `disable fork`, concurrent assertions, `force` / `release`.
- Timing controls in processes: `iff` qualifiers ([#1444](https://github.com/celox-sim/celox/issues/1444)),
  package-scoped delay values
  ([#1710](https://github.com/celox-sim/celox/issues/1710)), impure function calls in event expressions, and
  `wait` or event expressions reading a static task argument.
- Nonblocking assignments inside `always_latch` ([#431](https://github.com/celox-sim/celox/issues/431)); use blocking assignments for the supported latch subset.
- Inferred latches inside `always_comb` are rejected by default. Set `inferred_latch = "warn"` or `"allow"` in `celox.toml` under `[diagnostics]` to permit retained blocking assignments. Ordinary `always @*` and explicit `always_latch` do not require this option.
- Ports and instances: non-ANSI port declarations ([#426](https://github.com/celox-sim/celox/issues/426)), `ref` ports
  ([#427](https://github.com/celox-sim/celox/issues/427)), wildcard `.*` connections ([#442](https://github.com/celox-sim/celox/issues/442)), gate primitives ([#457](https://github.com/celox-sim/celox/issues/457)),
  `bind`.
- Declarations: variable declaration initializers of members of an interface
  array, ANSI port default values, tagged unions, soft packed unions, packed union assignment patterns, internal nets without a driver ([#460](https://github.com/celox-sim/celox/issues/460)),
  unpacked structs and `real`.
- Sequential processes: `iff` qualifiers ([#452](https://github.com/celox-sim/celox/issues/452)) in event lists, and in
  `always_ff` edge operands other than a plain signal ([#464](https://github.com/celox-sim/celox/issues/464)), multi-bit
  edge signals, and several edges without an asynchronous reset.
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
  `$fgets`, is
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

### Strings {#strings}

A `string` variable holds a reference to text in the simulator's heap, so
assigning, comparing for equality and detecting a change of a string cost the
same as for a 32-bit integer; operations that read or build text call into the
runtime. Strings are collected between time slots and after each `tick`.
As IEEE 1800-2023 6.16 requires, NUL bytes are removed when text becomes a
string, and an integral value assigned or cast to a string contributes its
bytes from the most significant one.

Not yet supported: `$sformat` / `$sformatf`, `atoreal` / `realtoa`, string
arguments of `$value$plusargs`, file I/O and DPI-C, and string values in VPI,
VCD output and the TypeScript API.

### Classes {#classes}

A class variable holds a handle of an object in the simulator's heap, `null`
until a constructor runs. Like a string, a handle costs the same as a 32-bit
integer to copy or compare; reading or writing a property and creating an
object call into the runtime. Objects that no variable or other object refers
to are collected between time slots and after each `tick`. A method is called
with the object it was called on, so it can be a function or a task, and
calls are bound to the method of the variable's class (IEEE 1800-2023 8.6).
A call of a virtual method runs the override of the object's class instead,
except `super.m(...)`, which runs the base's implementation (8.15, 8.20); it
selects the override by the class of the object, so it costs one more call
into the runtime. Reading or writing a property or calling a method through
`null` is a runtime error. Constructing an abstract class and a class that
leaves a pure virtual method unimplemented are rejected (8.21). A class that
implements an interface class implements each of its methods with the same
return type, or, if it is abstract, re-declares it pure (8.26). A method called
through a handle of an interface class runs the implementation of the object's
class.

A handle may be assigned to a variable of its class, of a base class or of an
interface class it implements; any other assignment of a handle is an error
(IEEE 1800-2023 8.16, 8.26.5). `$cast(dest, source)` assigns a handle whose
object's class derives from or implements the class of `dest`, or `null`, and
returns 1; otherwise it leaves `dest` unchanged and returns 0, or, called as a
task, reports an error. A `$cast` between classes neither of which derives from
the other, unless the source is an interface class, always fails (8.16).

A constructor calls the constructor of the base class first, with the
arguments of `super.new(...)` or of the `extends` clause, then runs the
initializers of the properties its class declares, then its body (IEEE
1800-2023 8.17). A static property is a variable of the module that declares
the class, so each instance of the module has its own.

Checkpoints keep objects. State files record strings but not objects, so
saving a state file fails while any variable holds an object.

### Out-of-range selects

A run-time select that reaches past either end of a
vector reads `X` for the missing bits (zero in two-state simulation) and
writes only the bits that exist. A write to a constant part-select partly
outside the vector, by any procedural or continuous assignment, also writes
only the bits inside: `q[9:6] = v` on `logic [7:0] q` writes `v[1:0]` to
`q[7:6]`. A write wholly outside the vector has no effect. Reading such a
constant part-select, and writing one of an array element or of an inner
packed dimension that leaves its declared range, are rejected
([#88](https://github.com/celox-sim/celox/issues/88)).

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

### Latches

Explicit `always_latch` and ordinary `always @*` support retained blocking
assignments. Incomplete assignments in SystemVerilog or Veryl `always_comb`
require `inferred_latch = "warn"` or `"allow"` in the common
[`[diagnostics]` table](./celox-toml.md#diagnostics). The default is `error`.
The Rust `.allow_inferred_latches(true)` and JavaScript `allowInferredLatches: true`
options remain available; an explicit diagnostic setting takes precedence.
Latches work with ordinary `eval_comb` and with `initial` testbenches; a gate
that remains open follows changes to its data, and a closed gate retains its
last value. In four-state mode an uninitialized latch starts at X, and an X
or Z procedural `if` condition does not execute the true branch.

`always_comb` and `always_latch` also execute once at time zero; `always @*`
waits for a sensitivity change, including changes caused by initial blocks or
continuous drivers. Repeated `eval_comb` calls with unchanged sensitivity inputs
do not execute a self-reading process again. As with
`always_comb`, written expressions are excluded from the sensitivity of
`always_latch` (IEEE 1800-2023 9.2.2.2 and 9.2.2.3). Cross-process feedback
still requires the existing explicit loop configuration. The option does not
suppress Veryl analyzer errors for explicit combinational arithmetic feedback.
For a selected read such as `bus[0]`, `always @*` watches the whole variable,
while `always_comb` and `always_latch` watch the longest static prefix.
The latter include reads inside called functions; task calls contribute only
their argument expressions (IEEE 1800-2023 9.4.2.2 and 9.2.2.2.1).
Nonblocking latch assignments (`<=`) require NBA scheduling and remain
unsupported, including when the inference option is enabled.

### Event and level waits {#timing-semantics}

- An event control waits for a change that happens after it begins
  (IEEE 1800-2023 9.4.2). In `a = 1; @(a ^ b);` the write to `a` is not an
  event of the wait; the process resumes when `a` or `b` changes later.
  Icarus 13.0 wakes on the earlier write; Verilator 5.052 agrees with Celox.
- `posedge` and `negedge` of a multi-bit expression follow its least
  significant bit (Table 9-2): `@(posedge v)` ignores `v[3]`, and
  `@(posedge v[3])` follows it. Transitions through X and Z count as in that
  table, so `0` to `x` is a `posedge`.
- Continuous assignments, combinational processes and event expressions see
  a process's writes when that process reaches a timing control or ends.
  After `b = 1; a = 1;`, `a ^ b` has not changed, and a pulse
  `a = 1; a = 0;` wakes `@(a)` but not `@(a ^ b)`; write `#0` between the
  writes to make the pulse visible. IEEE 1800-2023 4.7 permits this order;
  Icarus evaluates the expression at each write.
- A waiter sees every value that one evaluation of `always_comb`,
  `always @*`, an edge-triggered `always` or `always_ff` writes, including
  one that a later write overwrites: with `a` at 1, `y = 0; if (a) y = 1;`
  wakes `@(y)` (IEEE 1800-2023 4.3, 9.4.2). Nonblocking updates apply in the
  NBA region in issue order, each its own update (10.4.2). A child's output
  port is a continuous assignment to the parent's net (23.3.3), so a waiter
  in the parent sees only the settled value, as in the previous item.
  Verilator 5.052 sees only the final value of an evaluation; Icarus 13.0
  also sees the pulse through the port.
- A blocking write of an edge-triggered `always` is an update of the active
  region: the processes it wakes run before the same edge's nonblocking
  updates. With `y = ~y; q <= q + 1;`, a waiter on `@(y)` reads `q` from
  before the edge and waits again before the nonblocking updates
  (IEEE 1800-2023 4.5). Icarus 13.0 gives the same values; Verilator 5.052
  resumes the waiter after the nonblocking updates.

### Variable lifetimes

Variable lifetimes follow IEEE 1800-2023 6.21. Outside classes, a variable
declared in a procedural block, function or task is static unless it, its
subroutine or its module (`module automatic`) is declared `automatic`.
Class methods have automatic lifetime (8.6). A static local of a block
keeps its value from one execution of the block to the next, and its
initializer runs once at time zero: in `always_ff`, `int count; count++;`
counts. An automatic local is initialized on every entry. A static local
whose value on entry is never read, because every path writes it before
reading it, behaves the same either way. Because a static local keeps its
value, a local of `always_comb` that only some paths write is a latch, and
requires relaxing `inferred_latch` like other inferred latches.

Static function locals and result variables keep their values between calls
(13.4.2), including scalar `real` values and packed/unpacked integral storage.
An explicit `static` local of an automatic function is shared too; automatic
locals are initialized on each call. Static output arguments retain their
previous values on paths that leave them unassigned. Observable static storage
is shared by call sites and timed processes, and module-local storage is separate
for each module instance. Declaration initializers run once at time zero,
including when the function has not been called. Initializers that call a
stateful function require `Simulation` or a native testbench to execute the
initialization process. Constant function calls use
fresh values and do not affect runtime storage (13.4.3).

Task locals that keep their values and nonconstant static initializers remain
unsupported; declare those locals `automatic`. Package function state follows
the existing package-variable restrictions on multiple procedural drivers.
An interface whose default lifetime (`interface automatic`) differs from that of the
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

### Binary input

`$fread` reads big-endian bytes into a packed integral destination or a
one-dimensional static integral memory (IEEE 1800-2023 21.3.4.4). Memory reads
start at the lowest index by default and proceed through increasing indices;
optional start and count operands select the range. Packed destinations ignore
those operands. Partial reads preserve bits that received no input, including
X/Z bits. The return value is the number of bytes read, or zero on an I/O error.

### Byte input and file positioning

`$fgetc(fd)` returns the next byte as an integer from 0 to 255, or -1 on EOF
or error. `$ungetc(character, fd)` pushes back one byte without modifying the
file and returns zero, or -1 on error. One byte of pushback is supported.
`$feof(fd)` tests whether a previous read detected EOF; reading the final byte
alone does not set it. Successful pushback and repositioning clear EOF.

`$ftell(fd)` returns the logical byte position, accounting for pushback, or -1
on error. `$fseek(fd, offset, operation)` accepts signed 32-bit offsets relative
to the start (0), current position (1), or end (2). `$rewind(fd)` seeks to the
start. Both positioning functions return zero on success or -1 on error;
successful seeks discard pushback. Position results beyond signed 32-bit range
return -1. `$fflush(fd_or_mcd)` flushes selected output channels; `$fflush()`
flushes all open files. Invalid flush descriptors produce a runtime error.
These operations run in timed `Simulation` processes and use file descriptors
for input and positioning; multichannel descriptors support output and flush.

### Formatted input

`$sscanf(source, format, outputs...)` and `$fscanf(fd, format, outputs...)`
support integral `%d`, `%h`/`%x`, `%o`, `%b`, and `%c` conversions and real
`%e`/`%f`/`%g` conversions, numeric field widths, assignment suppression,
literal matching and whitespace (IEEE 1800-2023
21.3.4.3). Source and format can be string literals or packed integral values.
The result counts assigned outputs; unmatched outputs retain their values.
Real conversions accept `real`, `realtime` and `shortreal` destinations, including
signed decimal inputs with an optional decimal point and exponent. `shortreal`
rounds directly to binary32; overflow produces signed infinity and negative zero
is preserved. Incomplete signs or exponents fail without assigning the output.
Integral conversions require integral destinations; real conversions require real
destinations. `%s` string outputs and `%t` time scaling are unsupported.
Both functions require timed `Simulation` execution. Pure `$sscanf` state can
be checkpointed; `$fscanf` uses external file state and prevents checkpoints.

### Runtime file output

`$fopen`, `$fclose`, `$fdisplay` / `$fwrite` and their `b` / `o` / `h`
forms execute in a timed `Simulation` (IEEE 1800-2023 21.3). A one-argument
`$fopen` returns a multichannel descriptor; OR descriptors to write to multiple
files, including standard output through bit zero. Supplying an open mode returns
a file descriptor, with the standard streams at `32'h8000_0000` through
`32'h8000_0002`. Closed channels are reused. Open failures return zero.
Invalid descriptors and write failures produce a runtime error. File names and
modes accept string literals or integral packed strings containing UTF-8 paths.
Dynamic `string` values and non-UTF-8 paths are unsupported. Relative paths use
the host process's working directory.

`$writememh` / `$writememb` snapshot static unpacked arrays when called,
overwriting the destination (21.5). Optional runtime start and finish addresses
select either traversal direction; out-of-range addresses are runtime errors.
The output retains X/Z bits, contains no address directives, and is readable by
`$readmemh` / `$readmemb`. Those existing tasks still read at compile time.
Dynamic and associative memory arrays are unsupported.

Edge procedures that call these operations, directly or through subroutines,
run as scheduled kernels. Build them with `Simulation`; a `Simulator` builder
rejects them. Calls from combinational processes are unsupported.

### Constant parameters of aggregate type

Constant parameters of aggregate type (unpacked arrays, packed structs
given by a pattern) are variables that hold their value from the start of
the simulation; they cannot be used where an elaboration-time constant is
required.

### Runtime I/O and plusargs

Use `Simulation::from_sv_sources` for host I/O in `initial` blocks and
edge-triggered `always` / `always_ff` processes, including their subroutines.
These calls are unsupported in combinational procedures. A bare `Simulator`
rejects edge procedures that need this timed execution.

Supply ordered command-line arguments with `.plusargs(["+COUNT=12", "+TRACE"])`.
`$test$plusargs` matches a prefix; `$value$plusargs` supports integer conversions
and `%s` into packed integral variables. The first matching argument wins, and
an unmatched value query preserves its destination. Real conversions and dynamic
strings are unsupported.

See [runtime file output](#runtime-file-output) for file descriptors and memory snapshots.

[Source VCD controls](./vcd#systemverilog-source-controls) start recording after
the current timestamp settles. Checkpoints and state files reject designs with
file I/O or source VCD controls, whose host state they cannot capture.

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
`Simulator`, which has no time, returns 0. `$realtime` returns a `real` in
the calling scope's time unit without integer rounding (20.3.3). For example,
at 1.25 ns in a scope with a 1 ns time unit, `$realtime` returns 1.25 and
`$time` returns 1. Real values have IEEE 754 double precision; large tick
counts may lose precision when converted to `real`. Time queries follow the
simulation clock when a checkpoint is restored.

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

`$dist_normal`, `$dist_exponential`, `$dist_poisson`, `$dist_chi_square`,
`$dist_t` and `$dist_erlang` use the Annex N distributions in timed
`Simulation` processes and their subroutines. Arguments and results are signed
32-bit integers; the integral inout seed is converted to an int and copied
back once, including when its actual is an indexed element. Exponential and
Poisson means, chi-square and t degrees of freedom, and both Erlang parameters
must be positive; invalid parameters report a runtime error without updating
the seed. Normal accepts any integer mean and standard deviation. These
functions do not require an SV `real` type. Their seed is design state, so
checkpoints replay their sequence.

## Real mathematical functions {#real-math}

`$ln`, `$log10`, `$exp`, `$sqrt`, `$pow`, `$floor`, `$ceil`, `$sin`, `$cos`,
`$tan`, `$asin`, `$acos`, `$atan`, `$atan2`, `$hypot`, `$sinh`, `$cosh`, `$tanh`,
`$asinh`, `$acosh` and `$atanh` accept real arguments and return a real result,
following the C math functions in IEEE 1800-2023 20.8.2. Integral and shortreal
arguments are converted to real before the call; `$floor` and `$ceil` also
return real, and `$atan2(y, x)` preserves that operand order. Domain and range
cases produce the corresponding floating-point NaN or infinity.

These pure functions work in combinational, clocked and timed processes, and
with constant arguments at elaboration (11.2.1). Real parameters retain their
precision through nested calls; integer assignments and casts convert only the
final result. All simulator backends use the same math operation evaluator.

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
