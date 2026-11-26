---
outline: [2, 3]
---

# SystemVerilog Conformance and Deviations

For setup and a first simulation, start with [SystemVerilog Support](./systemverilog).

## Baseline: IEEE 1800-2023 {#what-is-supported}

Celox implements SystemVerilog as specified by IEEE Std 1800™-2023.
This page does not restate the standard.
It organizes every difference into five categories:

1. **Unsupported features** — entire constructs or clauses not implemented and rejected at compile time.
2. **Partially supported features** — features where Celox implements a specific subset; each table states **Supported** and **Not supported** side by side.
3. **Narrow limitations** — specific edge-case restrictions in features that otherwise follow the standard.
4. **Intentional deviations** — split into [deviations affecting standard-conforming code](#deviations-standard-code) and [permissive acceptance of non-standard code](#deviations-nonstandard-code).
5. **Implementation-defined choices and the Celox execution model** — tool choices where the standard leaves behavior unspecified, plus Celox's Rust `Simulator` / `Simulation` split, host clocks, multiple top modules, checkpoints, memory files, mixed-language designs, and DPI-C registration.

### Conformance scope: standard vs. non-standard code

- **Standard-conforming code:** Valid IEEE 1800-2023 code that does not rely on any construct, limit, or deviation listed on this page executes as the standard specifies — or, if a feature is unimplemented, fails at compile time with an `Unsupported` diagnostic naming the construct and its tracking issue (or the frontend roadmap [#88](https://github.com/celox-sim/celox/issues/88)) rather than silently mis-simulating. Any unlisted difference on valid standard code is a bug; please [file an issue](https://github.com/celox-sim/celox/issues).
- **Non-standard code (default mode vs. strict mode):** Accepting non-standard code is less severe than mis-simulating valid standard code, so **default mode** intentionally accepts several common compatibility relaxations with warnings (see [Permissive acceptance of non-standard code](#deviations-nonstandard-code) and [`[diagnostics]`](./celox-toml.md#diagnostics)). In **strict mode** (`strict = true` / `--strict` / `.strict(true)`), those compatibility diagnostics become errors; **accepting any non-IEEE 1800-2023 code in strict mode (outside the explicit extensions documented below) is a bug** — please [file an issue](https://github.com/celox-sim/celox/issues).

Measured conformance is published on the [verification page](/verification/).

## Unsupported features {#what-is-not-supported}

These constructs and clauses are not implemented and are always rejected at compile time:

- **Verification and design-level blocks:** `bind`, `checker`, `config`, clocking blocks and clocking drives, user-defined primitives (UDP), and `specify` blocks. Covergroups are supported in part; see [Coverage groups](#covergroups).
- **Unions and mixed array/container types:** `tagged` unions, unpacked unions, packed union assignment patterns, fixed-size unpacked arrays of `chandle` (`chandle a[4]`, including via `typedef`), fixed-size unpacked arrays of dynamic containers (`int a[2][$]`, including via `typedef`), and dynamic containers of multidimensional fixed-size unpacked arrays (`int a[][2][3]`).
- **Control flow:** `disable` statements other than `disable fork`; `fork` inside declaration initializers, `final`, `always_comb`, and `always_ff` (no process scheduler in those contexts).
- **Timing and system tasks:** `wait_order`, and any unimplemented IEEE 1800-2023 or Annex D system task/function. Celox knows every standard name and reports unsupported ones by name wherever written, including in unused parameters and function bodies (e.g., `$q_initialize`).

## Partially supported features {#partial-support}

For features with partial support, each table states both the **Supported** subset and what is **Not supported**.

### Ports, nets, `defparam`, and gate primitives {#ports-and-gates}

| Feature | Supported | Not supported |
| --- | --- | --- |
| **Wildcard `.*` port connections** ([#442](https://github.com/celox-sim/celox/issues/442)) | Wildcard `.*` connections to modules defined in the SystemVerilog sources (including modules with interface ports) | Wildcard `.*` connections to a module defined outside the SystemVerilog sources (such as Veryl) |
| **`ref` and `const ref` ports (23.3.3.2)** | ANSI and non-ANSI `ref` and `const ref` ports on modules and programs: packed/unpacked/struct/enum/`string`/`real`/class/container/`event` variables and unpacked-struct members, chained `ref` hierarchies, package variable actuals, `const ref` read-only enforcement, and IEEE 1800-2023 6.22.2 semantic type-equivalence checks | Top-level `ref` ports; connections to individual elements of unpacked arrays or dynamic containers; instance-array element-by-element slicing; queues with differing bounds; procedural `force`/`release` or `assign`/`deassign` targeting `ref` ports (or variables aliased by them); `ref` ports crossing a Veryl/SystemVerilog boundary |
| **Nets with multiple drivers (6.6.1–6.6.3)** | Continuous assignments within one module to `wire`, `tri`, `wand`, `triand`, `wor`, or `trior` nets (resolved per 6.6.1–6.6.3. In 2-state simulation, without X/Z literals). Continuous assignments and net declaration assignments to `wire` or `tri` resolve drive strengths (28.12) | Multiple drivers across module instances (such as a tri-state bus across modules); drive strengths on `wand`, `triand`, `wor`, and `trior` with multiple drivers (28.12.4); `tri0`, `tri1`, and supply nets with several drivers; drivers calling a function with a data-dependent loop; in 2-state simulation, drivers with X or Z literals |
| **Undriven nets and `inout` ports** | Sub-module `inout` ports with one continuous driver on either side (e.g., `assign io = oe ? d : 'z;`); undriven bits of a net with an independent delay read `z` | Top-module `inout` ports; internal nets without a driver ([#460](https://github.com/celox-sim/celox/issues/460)) except undriven bits of a net with an independent delay |
| **Gate and switch primitives (Clause 28)** | `and`, `nand`, `or`, `nor`, `xor`, `xnor`, `buf`, `not`, `bufif0`, `bufif1`, `notif0`, and `notif1` without delays or drive strengths, single or in 1D instance arrays | All other gate and switch primitives (switches, pull sources); gate delays, drive strengths, and multidimensional gate instance arrays ([#457](https://github.com/celox-sim/celox/issues/457)); a net that a three-state gate (`bufif0`, `bufif1`, `notif0` or `notif1`) drives has no other driver, as their L and H outputs resolve by strength (28.12.2) |
| **`defparam` assignments** | Overriding a parameter of a direct child module instance of the module containing the `defparam` (including inside a generate block) | Paths through a child instance (`a.b.P`), paths starting at `$root`, elements of instance arrays, interface instances, and parameters of the statement's own scope |
| **`let` declarations (11.12)** | Declarations among items of a module, interface, or program, expanded at each call | `let` in a package, generate block, or procedural block; calls in a scope that redeclares a name used in the `let` body |

### Hierarchical references {#hierarchical-references}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Variables, nets, and continuous assignments** | Reads of nets and variables, procedural writes (including unpacked array elements and `string` variables/methods), members of a packed-struct variable or of an element of an array of packed structs, and continuous assignments driving another instance's variable (when its own module does not also drive it continuously or from `always_comb`) | Continuous assignments driving a net through a hierarchical name, or driving a variable that its own module also drives continuously or from `always_comb`; hierarchical references in port connections; named blocks; structure members of another scope other than packed structs or arrays of packed structs; `real` or class-typed signals; names starting inside a generate block relative to it |
| **Subroutine calls (23.8)** | Calling a task or function declared in another module's scope when it resolves to one specialization for every instance of the calling module | Subroutines whose formals, result, or named variables are `real`, `shortreal`, class, or `event` typed; two modules calling each other's subroutines |
| **Parameters (23.6)** | Reading an integral parameter or localparam of another instance (`u.N`) with at most one `[N-1:0]` packed dimension (values > 128 bits must have no X/Z bits) as a run-time value | Non-integral or multi-packed-dimension hierarchical parameters; values > 128 bits with X/Z bits; using a hierarchical parameter as an elaboration-time constant expression (A.8.4 `constant_primary`) |
| **Static subroutine variables (`f.v`, 23.6)** | Accessing a subroutine's static variable from a scope where `f` is visible by simple name (a method of the class or a base, or a subroutine of the design element) | Access through an instance, package, or named block; static variables of unpacked structure type |
| **Upward names from interface bodies (23.8)** | Resolving undeclared names in an interface instance body (including in tasks/functions called via a virtual interface) from the instantiating scope, when the name denotes objects of one type in every instance of the interface | Upward lookup when reached through an interface port, or inside an interface instance declared within another interface's body |

### Interfaces {#interface-restrictions}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Declarations** | Value and type parameters, header package imports, `localparam`, `typedef`, variables, nets, `assign`/`always_comb`/`always_ff`/generate logic, functions, tasks, and nested interface instances up to 64 levels | Implicit nets, `enum`, module instances, macros, and compiler directives inside an interface; recursive specializations or nesting beyond 64 levels; variable declaration initializers on members of an interface array; default lifetime (`interface automatic`) differing from the using module |
| **Modports and connections** | `input` and `output` modport members, `import` functions/tasks, generic `interface` and `interface.mp` ports, explicit/implicit named connections (`.bus(bus)`, `.bus`), and read-only access through a port without a modport | `inout`, `ref`, expression ports, `export`, and clocking modports; naming a modport in a connection when the receiving port declares none; writes through a port without a modport; member connections from a modport-less port to a non-ANSI module |
| **Dependencies, names, and identifiers** | Standard scoping within one compilation order and plain identifiers without `$` | Member references in parameter, constant, type, or member declarations of an interface used through a port; `$unit` items used by an interface when a module in another file or earlier in the file uses it; references to implicit generate names (`genblk1`) and paths through generate blocks (`g.h.x`); declarations reusing an interface instance or port name; package names imported via an interface colliding with another imported package item or module declaration; user identifiers containing `$` or non-simple escaped identifiers in designs with interfaces (`$` is reserved for interface expansion) |

For static-variable storage of interface tasks and functions called across modules ([#3739](https://github.com/celox-sim/celox/issues/3739)), see [Deviations affecting standard-conforming code](#deviations-standard-code).

### Virtual interfaces {#virtual-interfaces}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Declarations, parameters, and access** | `virtual ifc`, `virtual ifc.mp`, and `virtual ifc#(P)` for interfaces with integral constant value parameters; variables, class properties, subroutine arguments/results, array/container elements, class type parameter values; reads/writes to variables; calling interface tasks and functions *(see storage note under [Deviations affecting standard-conforming code](#deviations-standard-code))* | Virtual interfaces of interfaces with type parameters or non-integral-constant parameter values; clocking blocks and nested interface instances reached through a virtual interface; writes to nets through a virtual interface; `$bits` of an interface function call through a virtual interface in a package or class (works in a module; [#4056](https://github.com/celox-sim/celox/issues/4056)) |
| **Event controls and `wait`** | `@(...)` and `wait` through a virtual interface held in a static variable, a property of `this`, or a property of a statically held object; named-event waits `@(v.e)` and `@(v.e or other_event)` (waiting on the event denoted when the wait begins) and triggers | `wait` through a virtual interface held by an automatic variable, subroutine argument, or class property of an automatically held object; event controls mixing a virtual-interface named event with other kinds of events (`@(v.e or v.d)`) |

### Programs {#programs}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Program declarations (Clause 24)** | Instantiated `program` blocks with ports and parameters; their `initial` procedures run in the reactive region set | Programs nested in modules; anonymous programs; uninstantiated programs as implicit top levels; clocking blocks inside programs |

Program region and `$exit` differences are listed under [Deviations affecting standard-conforming code](#deviations-standard-code).

### Unpacked structures {#unpacked-structures}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Storage and contexts** | Module variables, procedural locals, non-`static`/non-`rand`/non-`const` class properties, dynamic container elements, and subroutine arguments (including `ref`) and return values | Module ports, nets, and parameters; `static`, `rand`, or `const` class properties |
| **Scalar and nested members** | Integral types, enums, `string`, class handles, packed structs, and nested unpacked structs (each keeping its own 2-state/4-state type) | `real` / `shortreal` members |
| **Fixed-size array members** | Fixed-size unpacked arrays (including multidimensional and typedef'd) of plain integral or packed-vector types, in module variables and procedural locals only | Fixed-size array members of enum, packed aggregate, `string`, handle, or unpacked struct type; declared initial values on fixed-size array members; structs with fixed-size array members as ports, class properties, subroutine interfaces, or dynamic container elements |
| **Dynamic container members (7.2.1)** | Dynamic array, queue, and associative array members (copied with the structure) | Declared initial values on container members; fixed-size arrays of dynamic containers (`int a[2][$]`); `%p` of a struct with a container member; fixed-size arrays of such structs; mutating a container member of a struct stored inside another container (`q[i].m.push_back(x)`, `q[i].m[j] = x`, `q[i].m = x` — read-only there) |
| **Fixed-size arrays of structs** | Module variables and procedural locals where every leaf member is integral, all leaves are 2-state or all are 4-state, and no member declares an initial value | Structs with `string`, handle, or dynamic-container members, mixed 2-state/4-state leaves, or member initial values; arrays of structs as ports, class properties, or subroutine interfaces |
| **Member initial values (7.2.2)** | Constant integral initializers on module variables, procedural locals, and class properties (resolved in the declaration scope) | Initializers on enum, non-integral, or array/container members; whole nested-struct member initializers; defaulted struct types in module ports, subroutine interfaces, or dynamic containers |

### Procedural `force`/`release` and `assign`/`deassign` {#procedural-force-and-assign}

| Construct | Supported | Not supported |
| --- | --- | --- |
| **Procedural `force` / `release` (10.6.2) — placement and targets** | Inside `initial`, `always` (runs as a process), and tasks; targets are packed integral module variables/nets (including via hierarchical names and interface instance/array members `u.bus[2].ready`), constant bit/part-selects or members, and constant-indexed elements (or constant selects/members) of fixed-size unpacked arrays of packed integrals *(variable selects and unpacked array elements are non-standard extensions — see [Permissive acceptance of non-standard code](#deviations-nonstandard-code); emits `forced_array` / `forced_specialization` warnings where applicable)* | In functions, `final`, `always_comb`, `always_latch`, or `always_ff`; concatenations; selects with non-constant bounds; unpacked array slices; `real`, `string`, handle, or unpacked-struct targets; top-module input ports; targets also forced by host `Simulator::force`; interface members written by another instance through an interface port; unpacked arrays written by several edge-triggered processes; variables written by delayed NBAs; targets read and written by the same `always_comb`/`always_latch`/`always @*` block or assigned whole by an `always_comb` running as a process; overlapping forced parts when one force RHS reads a variable |
| **Procedural `force` / `release` — RHS expressions** | Constants, module/instance signals, static task variables/arguments (read when `force` executes), and `$urandom`/`$urandom_range` with constant arguments (drawn on execute and when a RHS variable changes) | Expressions with side effects, subroutine calls, or system functions other than `$urandom`/`$urandom_range` with constant arguments; expressions reading procedural block locals or automatic task variables |
| **Procedural `assign` / `deassign` (10.6.1)** | Inside `initial` and `always` (runs as a process), targeting a whole packed integral module variable with a side-effect-free, call-free expression over module signals *(see [Deviations affecting standard-conforming code](#deviations-standard-code) for override timing on indirect writes)* | In subroutines, `final`, `always_comb`, `always_latch`, or `always_ff`; concatenations, `real`, `string`, handle, unpacked, or procedural-block-local targets; targets also assigned by `always @*`; RHS with side effects, subroutine/system-function calls, or reads of procedural block locals |

Forcing an instance's input port forces that port rather than the connected net; a write to a forced target through a hierarchical name from a module that does not force it is not overridden; and a run-time-indexed write to a forced unpacked array preserves the forced bits of that element.

### Delays {#continuous-assignment-delays}

| Form | Supported | Not supported |
| --- | --- | --- |
| **Intra-assignment timing (10.4.2)** | Delayed nonblocking assignments (`lhs <= #constant rhs`) with integral constant delays or time literals in `initial` and ordinary `always` processes | Nonblocking intra-assignment event controls (`lhs <= @(e) rhs`); non-constant delays; delayed NBAs outside `initial` and ordinary `always` |
| **Continuous assignment delays (`assign #d`, `wire #d w = expr`)** ([#444](https://github.com/celox-sim/celox/issues/444)) | One nonnegative integral constant or time literal on a scalar or packed integral target (including constant bit/part-select) with an integral RHS containing no subroutine calls | Rise/fall/turn-off delay lists; variable, negative, or unitless fractional real amounts; concatenated or aggregate destinations; dynamic destination selects; non-integral destinations; RHS containing subroutine calls |
| **Independent net delays (`wire #d w;`)** | One nonnegative integral constant or time literal on a scalar or packed integral net driven by static continuous assignments or supported logic gates (undriven bits read `z`) | Delayed nets in instance port connections; delayed `input` or `inout` ports; unpacked or non-integral delayed nets; drivers with dynamic destination selects |

### Event controls and waits {#timing-controls}

| Construct | Supported | Not supported |
| --- | --- | --- |
| **General `@(...)` event expressions** | Signals, `posedge`/`negedge`/`edge`, bit/part/dynamic selects, general expressions with pure functions, `iff` (in ordinary `always`/processes), and static virtual interfaces | Impure function calls in event expressions; reading a static task argument |
| **Class properties in `@(...)`** | A bare property item such as `@(posedge h.p)`, optionally with an `iff` reading only constants and variables written solely by the waiting process | Class properties inside a larger expression, through a function, or in a container held by a property; an `iff` on a property item reading anything written outside the waiting process; an expression item, or an `iff` other than a 1-bit variable, in the same event list as a property item |
| **`wait (condition)`** | General expressions, static virtual interfaces, and class properties (re-checked on matching property stores; see [Event scheduling](#timing-semantics)) | Conditions reading a static task argument, `rand_mode()`, or `constraint_mode()`; conditions over class properties that call a task or effectful function, or read time, a random number, a mailbox, or a process |
| **Fork-shared automatic locals** | Event expressions watching fork-shared automatic locals | Compound event expressions with mixed global/static dependencies, foreign calls, functions retaining scratch state, or implicit writes to a watched operand from a function, seed/plusargs result, or system task such as `$readmemh` |
| **`always_ff` event lists** | Single plain 1-bit signal edge (`@(posedge clk)`), clock plus asynchronous reset edge(s) tested in the body, and expression/member/bit/`edge` events (run as a timed process; see [Execution model](#celox-execution-model)) | `iff` qualifiers ([#452](https://github.com/celox-sim/celox/issues/452)); multi-bit edge signals; multiple edges without an asynchronous reset *(ordinary `always` supports these shapes as a timed process)* |

### Named events and mailboxes {#named-events}

| Feature | Supported | Not supported |
| --- | --- | --- |
| **Named `event` (15.5)** | Scalar event variables, class properties, and subroutine arguments; `-> e`, `->> e`, `@e` / `@(e1 or e2)` (including expressions such as `obj.e` and virtual-interface events `v.e`), `wait (e.triggered)`, `e1 = e2`, `null`, and equality comparisons | Arrays of events; event ports; event members of unpacked structures; functions returning an `event`; triggers in `always_comb`/`always_latch`; `->>` with a delay or event control; event controls mixing an event with signal changes or edges (`@(e or posedge clk)`); `triggered` combined with other operands or read via a function in `wait`; `wait_order` |
| **Mailboxes (15.4)** | Parameterized `mailbox #(T)` / `std::mailbox #(T)` where `T` is an integral type up to 64 bits, `string`, or a class, `mailbox`, `semaphore`, `process`, or `event` handle | Typeless `mailbox` (without a type parameter); element types wider than 64 bits or outside the types listed |

### Classes {#classes}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Parameters and specializations (8.25)** | Value and type parameters (including typedef'd enums/structs/unions, package and `$unit` classes, and forwarding a class's own 4-state/`string` parameters by name); interface classes (`implements` / `extends`) | Anonymous enum, struct, or union types as value or type parameters; named 4-state or `string` constants of a module or package as class parameter values; module-local classes as type arguments to package-class specializations |
| **Properties and member access** | 2-state/4-state packed integrals, enums, `string`, class handles, `chandle`, virtual interfaces, named events, non-`const` dynamic containers, non-`static`/non-`rand`/non-`const` unpacked structs (without fixed-size array members), and 1D fixed-size array properties (see row below); bit/part-select reads on `[N-1:0]` properties (any declared range inside constraints) | `real` properties; `const` dynamic container properties; packed-struct members of a property; selects of a member of an unpacked-struct property; bit/part-select reads outside constraints on properties not declared `[N-1:0]` |
| **Fixed-size array properties** | Non-`static`, non-`randc` 1D fixed-size array properties with nonnegative `int` bounds; element access, assignment/comparison with another such property or a directly named array variable (for non-struct elements), and `[0:N-1]` constraint `foreach` / unpacked-struct locator methods | `static` or `randc` fixed-size array properties; > 1 unpacked dimension, negative bounds, or bounds outside `int`; assigning/comparing with arrays other than another such property or a directly named array variable (or any array variable for unpacked-struct elements); keyed assignment patterns; unpacked array concatenations containing an array item; slices; passing to `ref`/`output`/`inout` formals; constraint `foreach` or unpacked-struct locator methods when the range is not `[0:N-1]` |
| **Assignments, arguments, and overrides** | Blocking assignments to properties; passing whole properties as `input` or `ref` arguments (see [Reference arguments](#reference-arguments)); module classes overriding virtual methods of external classes when reading non-array integral module variables/ports or class handles | Nonblocking assignments to properties; properties as `output` or `inout` arguments; virtual method overrides of classes declared outside the module that read module variables/ports of other types (such as unpacked arrays), static variables, or call module subroutines reading module variables |
| **Virtual method dispatch (8.20)** | Calls and `$cast` dispatch on the class of the object in every scope, also for objects of classes of packages the calling scope does not use, and in the code of interface instances bound into such a scope | A virtual method call, in another scope, on an object of a class a module declares or of a specialization a module makes of a package class template: a fatal error naming the method, and the `$cast` of such an object fails |
| **`$cast` (6.24.2)** | Dynamic `$cast` between class handles | `$cast` to a variable that is not a class handle |

### Randomization, constraints, and `randsequence` {#randomization}

| Feature | Supported | Not supported |
| --- | --- | --- |
| **Scope `randomize()` / `std::randomize()` (18.12)** | Unconstrained whole packed variables (any width, signed/unsigned, packed structs) and scalar enums up to 64 bits (known declared values); `with` constraints on integral variables ≤ 64 bits | `with` constraints on non-integral or > 64-bit variables (such as enums); enums with unresolved/X/Z values; unpacked arrays, nets, constants, selects, and concatenation arguments |
| **Class `randomize()` and `randc` (18.3–18.4, 18.8, 18.11)** | 2-state and 4-state integral `rand` properties (any width unconstrained; 4-state `rand` takes 2-state values), scalar enums (declared values), fixed-size and dynamic random arrays of 2-state integrals (a constraint may name the size of an array of elements > 64 bits, not its elements), inherited properties, `pre_randomize`/`post_randomize`, `rand_mode()`, `srandom()`, `get_randstate()`/`set_randstate()`, explicit member lists, `randomize(null)`; unconstrained `randc` on 1–16-bit 2-state integers and scalar enums up to 65,536 members | Aggregate properties (static or non-static); `randc` static properties or `randc` arrays; random arrays of elements other than 2-state integrals; constraints on elements > 64 bits of random arrays; random object or `string` properties; `randc` domains > 16 bits or > 65,536 enum members; enums with unresolved/X/Z values |
| **Constraint blocks and expressions (18.5)** | 1–64-bit scalar integer properties (constant bit/part-selects of any declared range), comparisons, logical/bitwise ops, `+`, `-`, `*`, `**` (Table 11-4; `0 ** -1` is X in 4-state simulation and 0 in 2-state); shifts; `if`/`else` and `->` (state guards evaluated before solving); `inside` (scalar values, inclusive ranges, and array properties of integral elements, which stand for their elements); `foreach`, `sum()`, and constant-index elements on random arrays (array sizes solved before elements/function calls; past-the-end reads 0); array elements at positions that state expressions give (`a[i + 1]`, `b[i]` in a loop over `a`; X/Z positions read 0); `dist` (constant weights/range bounds, `$` type bounds); `unique` on singular class properties of equivalent types; `soft` constraints and `disable soft`; `solve ... before`; `constraint_mode()`, inheritance, overrides; `==`/`!=` on class handles; single-hop handle members (`h.m`, `this.h.m`, `super.h.m`, `item.h.m`, static handles, selects `h.m[3:0]`/`hs[i].m[0]`); class functions with input arguments (18.5.11); the bit-vector functions `$countones`, `$onehot`, `$onehot0`, `$isunknown`, and `$countbits` with constant control bits (20.9), also in `foreach` | Constrained enums or `randc`; 4-state static properties read as state, 4-state members of other objects, or 4-state array elements in constraints; constraints naming static properties in a `rand`-handle-reached object; `sum() with`; `soft` inside `foreach`; array positions that a random variable decides; `/` and `%`; `dist` with `default :/`, non-constant weights, or `:/` ranges with non-constant bounds; `unique` on other variables, arrays, or slices; deeper handle chains (`h.g.m`); `$countbits` control bits that are not constants or that count `'x` without `'z` (or `'z` without `'x`); class function calls in constraint loops, inline constraints, `rand`-handle-reached objects, or arguments of another constraint call; typed parameters in constraints; associative arrays, arrays of the caller, nested arrays, X/Z wildcard items, or `$` bounds in `inside` sets |
| **Inline `randomize() with { ... }` (18.7)** | Same constraint subset; unqualified receiver properties, receiver handle variable (`h.randomize() with { h.x > 0; }`, read once at call start), caller-scope scalar inputs and expressions computed from caller variables and non-random receiver properties (`buffer[i]`, `req.rw`, `memory[rsp.addr]`, `m.get_size()`), and `local::` for non-property caller locals/arguments | Aggregate caller-scope inputs; typed parameter inputs; `local::` naming a property of the caller's class; caller expressions computed from a random variable (`memory[h.addr]` with `rand addr`); unqualified receiver `static` properties (rejected rather than falling back to a caller variable of the same name) |
| **`randsequence` (18.17)** | Non-recursive productions with weights, `rand join` (with bias; see [Random numbers](#random-numbers)), and `input` formals/return values of packed/scalar types | Recursive productions; code blocks after a weight; non-`input` formals; formals or values of unpacked, dynamically sized, or `event` types |

See [Random numbers](#random-numbers) for the solver policy, `soft`, `dist`, `solve before`, 4-state constraint semantics, constraint function calls, and the `rand join` bias formula.

### Dynamic containers {#dynamic-containers}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Container and nesting shapes** | Dynamic arrays (`T a[]`), queues (`T q[$]`, bounded `T q[$:N]`), associative arrays (`T m[K]`), nested dynamic containers (`int m[string][$]`, `T m[C][C][E]`), and dynamic containers of one-dimensional fixed-size unpacked arrays (`int a[][2]`, `bit [63:0] m[int][1]`: an element that takes its default, as a missing associative entry or one `new[]` adds, is an array of default elements, 7.4, 7.8.6) as module variables, locals, and class properties | Fixed-size unpacked arrays of dynamic containers (`int a[2][$]`, including via `typedef`); dynamic containers of multidimensional fixed-size unpacked arrays (`int a[][2][3]`) or of fixed-size arrays of unpacked arrays; container ports; `const` containers |
| **Element types** | Packed integrals of any width, `string`, class handles, `chandle`, supported unpacked structs (no fixed-size array or defaulted members), and nested dynamic containers | `real` / `shortreal` elements; named `event` elements |
| **Associative array keys** | Integral types of any width (including packed structs and enums), `string`, class handles, and `process` handles | Wildcard `[*]`; `real`, unpacked types, or other non-integral/non-handle key types |
| **Literals, `new[]`, and assignments** | Positional patterns, `{}` / element concatenations, `new[n]` with another compatible dynamic array/queue or an assignment pattern (including `'{default: v}` for scalar/non-container/non-struct elements), default-only associative literals (`'{default: v}`, 7.8.6), and blocking element or bit/part-select writes (integral ≤ 64 bits, side-effect-free container/index) | `new[n](fixed_array)`; `new[]` assignment pattern mixing `default:` with other keys or for container/unpacked-struct elements; explicit index entries or mixed index/`default:` associative literals; slices; keyed patterns; associative arrays inside concatenations; nonblocking element assignments; compound element assignments whose container or index has side effects or whose element is a `string`, handle, or container; writing a member of an unpacked-struct element directly (assign the whole element) |
| **Ordering and comparison (7.12.2)** | `reverse` and `shuffle` (dynamic arrays, queues, and 1D fixed-size arrays of ≤ 64-bit non-struct elements); `sort` and `rsort` (integral ≤ 64 bits and `string` without `with`; unpacked structs, class handles, and elements > 64 bits with a `with` expression on dynamic arrays/queues); `==`/`!=`/`===`/`!==` on containers, also of elements > 64 bits and of unpacked structs (an X or Z bit makes `==` `x` unless a known bit differs, 11.4.5) | `sort`/`rsort` without `with` on elements > 64 bits, unpacked structs, or class handles; ordering fixed-size arrays that are multidimensional, of unpacked structures, of elements > 64 bits, or with a `with` expression; comparison of containers of unpacked structs with dynamic-array, queue, or associative-array members; `==`/`!=` on containers of unpacked structs that mix 2-state and 4-state members or have 4-state members wider than 64 bits in all |
| **Reduction methods (7.12.3)** | `sum`, `product`, `and`, `or`, `xor` on 1D array variables or dynamic-container class properties (fixed arrays may be > 64 bits; dynamic containers ≤ 64 bits) | `with` expressions and iterator arguments; multidimensional arrays; dynamic containers with elements > 64 bits |
| **Locator methods (7.12.1)** | `find`, `find_index`, `find_first`, `find_first_index`, `find_last`, `find_last_index` (with `with`), and `min`, `max`, `unique`, `unique_index` (with or without `with`, integral ≤ 64 bits or `string`) on dynamic arrays, queues, associative arrays, and 1D fixed arrays; `item` / named iterator and `.index` | Index-returning locators on associative arrays with integral keys > 64 bits; locator methods in continuous assignments, on multidimensional fixed arrays, on `real` elements, or as discarded statements; `with` / iterator clauses outside this locator subset |
| **Queries and subroutine formals** | Outer `$size`, sequence `$left`/`$right`/`$low`/`$high`/`$increment`, `$dimensions`; `$bits(q.pop_front)` on integral queues; `input`/`output`/`inout` container formals with compatible container variable actuals of matching element type and queue bound | Type queries (`$bits`, …) on `string`- or class-valued queue pop results; element-dimension queries and associative bound queries; function return values of container type; class properties or container elements as `output`/`inout` actuals; actuals whose element type differs from the formal's |

### Strings {#strings}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Scalar `string` variables and methods** | Standard `string` operations and methods (except `atoreal`/`realtoa`), hierarchical names (including generate blocks), `inside` / `case inside` equality matching on strings, `$display`/`$sformatf`, DPI-C `string` arguments/results, `$value$plusargs` `%s` into a `string` destination, and file I/O (`$fopen`, `$fdisplay`/`$fwrite`, `$writememh`/`$writememb`, `$fgets`, `$ferror`, `$sscanf`/`$fscanf` source/format) | `atoreal` / `realtoa`; value range items (`["a":"z"]`) in `inside` / `case inside` on strings; `string` variable as the format argument of `$value$plusargs` (or non-`%s` conversion into a `string` destination); `string` values in VPI, VCD/FST output, and the TypeScript API |
| **Fixed unpacked arrays of `string`** | Assignment patterns, whole-array and subarray copies, indexed reads/writes, non-mutating element methods, and blocking character writes plus `putc`/`itoa`/`hextoa`/`octtoa`/`bintoa` on complete elements | Nonblocking character writes; method calls on whole arrays or subarrays; array-valued `string` subroutine arguments or return values |

### Reference arguments {#reference-arguments}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Qualifiers (13.5.2)** | `ref` and `const ref` on automatic subroutines and class methods (including virtual methods) | `ref static` |
| **Actual arguments** | Variables (scalars, vectors, `string`, class handles, dynamic containers, unpacked arrays, unpacked structs), class properties, fixed-size unpacked array elements, and unpacked struct members | Elements of dynamic arrays, queues, or associative arrays; elements of class properties |
| **Operations on `ref` formals** | Whole reads/writes, forwarding a property-bound `ref` to an `output`/`inout` argument, and selects on array-element `ref`s at compile-time or unrolled-loop indices | Partial (bit/part-select) writes to a formal bound to a class property or its member; selects on a formal bound to an array element at a run-time-only position |

### Deferred assertions, assertion controls, and sampled values {#sampled-values}

| Feature | Supported | Not supported |
| --- | --- | --- |
| **Deferred immediate assertions (`assert #0`, `assert final`, …)** | Module items and procedural deferred assertions whose action is a single display or severity task call | User subroutines or more than one statement in the action block |
| **Assertion control tasks (20.11)** | Zero-argument `$asserton` and `$assertoff` (stopping and restarting all assertions in module/design scopes) | `$assertkill`, `$assertcontrol`, action control tasks (`$assertpasson`, …), and level/scope/assertion arguments; calling `$asserton`/`$assertoff` in a package or `$unit` (assertions in package subroutines/classes are not controlled); calling them from `always_comb`/`always_latch` when any procedure other than `initial` or an edge-controlled one also calls them; combining them with multi-path concurrent sequences (`##[m:n]` or `or`) |
| **Sampled value functions (16.9.3)** | `$sampled` in processes, combinational logic, multiclocked sequences, and callees; `$past`, `$rose`, `$fell`, `$stable`, and `$changed` in edge-triggered processes (and callees) with an inferred clock | Explicit clocking event argument; use without a clock to infer; nested calls (`$past($past(a))`); operands reading an unpacked array; `$past`/`$rose`/`$fell`/`$stable`/`$changed` inside multiclocked sequences or with function-call operands in concurrent assertions |

### Concurrent assertions {#concurrent-assertions}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Directives and placement** | Module and generate `assert property`, `assume property`, and `cover property` with an explicit clock; procedural `expect (property_spec) action_block` (16.17) in processes and tasks with an explicit clock | Procedural concurrent assertions (`assert`/`assume`/`cover property` inside procedures); implicit or default clocking |
| **Single-clock sequences and properties** | Boolean properties; `p iff q` of two Boolean expressions (also as consequent of `\|->`, where `a \|-> b iff c` is `a \|-> (b iff c)`); fixed-delay sequences (`a ##1 b`, up to 1024 ticks) joined by `\|->` or `\|=>`; bounded cycle-delay ranges (`a ##[1:3] b`, including leading), `or`, `and`, `intersect`, and `b throughout s` (up to 1024 ways to match, as whole property or consequent of a single-path antecedent, without match items); `disable iff` on single- or multi-cycle sequences (16.12); local variables of built-in integral types and match items (in fixed-delay module/generate assertions) | Unbounded cycle-delay ranges (`##[1:$]`); repetition; sequence operator `within`; `intersect` of sequences that never match on one tick; multi-path sequences in an implication antecedent, in `expect`, or with local variables/match items; `disable iff` in `expect` or multiclocked sequences; `iff` with multi-cycle or named operands or chained `a iff b iff c` |
| **Until properties (16.12.12)** | `a until b`, `a s_until b`, `a until_with b`, and `a s_until_with b` of Boolean operands, as the whole property or as the consequent of `\|->` or `\|=>` with a Boolean antecedent, with `disable iff`, also as the body of a named property; attempts of a strong form still pending when the simulation ends fail then, those of a weak form neither pass nor fail (F.5.3.1) | Operands other than Boolean expressions (sequences, properties, instances of named sequences); an until property as an operand of another property operator (`(a until b) until c`, `iff`); until properties in `expect`; until properties in a design that calls an assertion control task (20.11) |
| **Multiclocked sequences (16.13)** | Whole-property sequences joining single-path singly clocked subsequences via `##1` or `##0` across clock changes (value event controls incl. 4-state edges, `iff`, static virtual interface members; parenthesized clocking groups up to 64 nested groups, 16.13.3); `$sampled` inside subsequences | Multiclocked sequences inside implications, with `disable iff`, or with local variables/match items; named events as clocks; operators other than `##1`/`##0` across clock changes; `$past`/`$rose`/`$fell`/`$stable`/`$changed` in any subsequence; nesting beyond 64 groups |
| **Named properties and sequences** | Parameterless declarations in the same module outside generate blocks, whose body stays within this subset; a named sequence without local variables may be an operand of another sequence (same clock or leading) | Formal arguments; declarations in other scopes or generate blocks; instances of named sequences clocked differently or with local variables inside a multi-cycle sequence |
| **Function calls in assertions (16.6)** | Pure (side-effect-free) function calls in operands, `disable iff` conditions, and match-item values (arguments receive sampled values; body reads current values, 16.5.1) | Impure function calls (functions with `output`/`inout`/`ref`/`const ref` formals, non-local writes, retained static state, side-effecting system calls, or string/class method calls directly or transitively); DPI import calls; function calls in operands of `$past`/`$rose`/`$fell`/`$stable`/`$changed` |
| **Action blocks** | Untimed blocking statements, report/severity tasks, and subroutine calls that do not suspend, perform nonblocking assignments, start or control processes, or trigger events; calls of functions that run `fork … join_none` on some path, such as UVM 1.2 reporting (a fork that runs stops the simulation with an error, since an assertion's actions are not called from an `initial` or `always` procedure, IEEE 1800-2023 13.4.4) | Timed or nonblocking action statements and a `fork` in the action itself; suspending, event-triggering, or process-controlling calls, including tasks that fork |

For `expect (property_spec) action_block` (16.17), the executing process makes one attempt starting from the next clocking event, decides each step on sampled values, runs the pass or fail statement (by default `$error`) in the Reactive region, and continues. Multi-path sequences pass at the first tick any path matches and fail at the first tick no path can still match; a missed antecedent succeeds vacuously at that tick.

### Coverage groups {#covergroups}

A covergroup (clause 19) is a type: declared in a module, a program, a package, the compilation unit or a generate block, or embedded in a class (19.4). `new` constructs an instance, with the arguments of the covergroup; `sample()` samples it, with the arguments of a `with function sample` declaration, and so does each occurrence of the clocking event of the covergroup (`covergroup cg @(posedge clk);`) for each of its instances (19.3); `get_inst_coverage()`, `get_coverage()`, `T::get_coverage()` and `$get_coverage()` give coverage, also of a coverpoint or cross (`c.cp.get_inst_coverage()`, `T::cp::get_coverage()`) and with the covered bins and the bins in two `int` variables (`c.get_coverage(covered, total)`); `start()`, `stop()` and `set_inst_name()` control an instance (19.8). The results are read through `Simulation::coverage()` (see [Coverage](#coverage)).

A coverpoint (19.5) is of an integral type of any width: with or without a label, with a type of its own, and with an `iff` guard, whose expression is evaluated first. Its expression and its guard read the arguments of the covergroup and of `sample`, then the members of the class of an embedded covergroup, then the names of the scope. Its bins are the automatic bins of 19.5.3, also those of an enumeration, or the bins it declares for values (19.5.1 to 19.5.6): one bin, a bin for each value (`bins b[]`) or a number of bins (`bins b[N]`) of a list of values and ranges, also ranges with a tolerance (`[a +/- b]`, `[a +%- b]`), of the values a `with` expression keeps, or of the elements of an array; `default` bins; `ignore_bins` and `illegal_bins`; `wildcard` bins; and a guard on any of them (`iff`). A bin value is a constant, an argument of the covergroup or a call of a function with these, which the construction evaluates, so the instances of one type may differ in their bins. A sample of an illegal value is reported as an error.

A cross (19.6) crosses two or more coverpoints of the covergroup, or variables, which get a coverpoint of their own: an integral variable or net, a property of the class of an embedded covergroup, or an argument of the covergroup or of `sample`. It has a bin for each combination of bins of its coverpoints that take part in their coverage, and its `bins`, `ignore_bins` and `illegal_bins` take the combinations their select expressions select: `binsof(cp)` and `binsof(cp.bin)`, with `intersect {...}`, `!`, `&&`, `||`, parentheses, and the identifier of the cross itself, which selects every combination. A cross and each of its bins may have a guard (`iff`). A sample of an illegal combination is reported as an error.

The definition may assign every member of `option`, each at the levels 19.10 gives it, with any expression of the scope of `new` as the value, and the members of `type_option` but `real_interval`, with a constant (19.7.1). A procedure reads and writes the members of `option` of an instance and of its items (`c.option.weight = 2;`, `c.cp.option.at_least = 3;`) and of `type_option` of the type and of its items, through the type or through an instance (`T::type_option.comment = "x";`, `T::cp::type_option.weight = 3;`, `c.type_option.merge_instances = 1;`), but for the members the standard lets only the definition set (19.7, 19.7.1).

A construct of the **Not supported** column is rejected where a `new` or a method of the covergroup is written, not where the covergroup is declared: a covergroup that nothing constructs is accepted whatever it declares, and so is a variable of its type. The exceptions are `!` before a parenthesized selection and a conditional cross set expression, which the parser rejects where the covergroup is declared. All of them point to [#4294](https://github.com/celox-sim/celox/issues/4294), which tracks what is left of clause 19.

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Declarations (19.3, 19.4)** | A covergroup of a module, a program, a package, the compilation unit or a generate block; a covergroup embedded in a class; variables of a covergroup type | `covergroup extends` (19.4.1); a covergroup declared in an interface, which is rejected with the interface in most positions, and always when it has a clocking event; one declared in a checker or in a loop generate block; constructing or sampling a covergroup type that a module takes as a type parameter (it declares variables there) |
| **Coverpoints (19.5)** | Coverpoints of an integral type of any width, with or without a label, with a type of their own, and with an `iff` guard | A coverpoint of a `real` type; one of an enumeration whose enumerators have x or z bits; a coverpoint of a signal of a clocking block (clocking blocks are not supported) |
| **Bins (19.5)** | The automatic bins of 19.5.3, also those of an enumeration; bins of values and ranges (`bins b = {0, [1:3], [8:$]};`) whose values are constants, arguments of the covergroup or calls of functions with these; bin arrays (`bins b[]`, `bins b[N]`); ranges with a tolerance (`[a +/- b]`, `[a +%- b]`) of integral operands; `default` and `default[]` bins; `ignore_bins` and `illegal_bins`; `wildcard` bins; a guard on a bin (`bins b = {1} iff (en);`); `with` filters (`bins b[] = {[0:255]} with (item % 3 == 0);`, `bins b = cp with (f(item));`); bins of a set expression (`bins b[] = S;`) that is an array parameter with integral elements, or a dynamic array, a queue or a constant array of the class of an embedded covergroup with integral elements of at most 64 bits | Transition bins and `default sequence` bins (19.5.2); `bins b[N] = default` (the standard does not say how the values are distributed); a `with` filter on a bin array, on `ignore_bins` or on `illegal_bins` whose values are constants and more than 1,048,576, such as `ignore_bins b = cp with (...)` on an `int` coverpoint (on one `bins` bin such a filter is supported, see [Coverage](#coverage)); a wildcard value of more than 65,536 ranges of values in a bin array or under such a filter; a set expression of any other form: an assignment pattern, a concatenation, a fixed-size array that is no parameter, an array of strings, handles or elements of more than 64 bits; a tolerance range with a `real` operand; a bin value that is not integral |
| **Crosses (19.6)** | Crosses of coverpoints and of integral variables or nets (a variable gets a coverpoint of its own), with or without a label, with an `iff` guard; `bins`, `ignore_bins` and `illegal_bins` of select expressions of `binsof(cp)` and `binsof(cp.bin)`, with `intersect {...}` lists whose values are constants, arguments of the covergroup or calls of functions with these, `!`, `&&`, `\|\|`, parentheses and the identifier of the cross (`bins all = X;`); a guard on a cross bin | `with` and `matches` in a select expression (19.6.1.2); cross set expressions and functions declared in a cross body (19.6.1.3, 19.6.1.4); `!` before a parenthesized selection (`!(binsof(a) && binsof(b))`) and a conditional cross set expression, both rejected where the covergroup is declared; a cross that lists one item twice; `binsof` of a coverpoint that the cross does not cross; `&&` and `\|\|` that alternate through more than 32 levels of parentheses; an `intersect` list that meets values only a sample sorts into bins (see [Coverage](#coverage)) |
| **Options (19.7, 19.7.1)** | Every member of `option` in the definition, at the levels 19.10 gives it; the members of `type_option` in the definition with a constant value: `weight`, `goal`, `comment` (a string literal), `merge_instances`, `distribute_first`, and `strobe`, which has no effect on a covergroup without a clocking event (19.3); in a procedure, the members of `option` of an instance and of its items, and of `type_option` of the type and of its items, through `::` or through an instance, read and assigned with `=` | `type_option.real_interval` (real coverpoints are not supported); `type_option.strobe` read in a procedure; a `type_option.comment` that is no string literal, such as a string parameter; a member assigned with another operator (`+=`, `++`) or with `<=`; a member read in the select of a place that an increment or a compound assignment writes (`arr[c.cp.option.weight]++`); `option` or `type_option` used as a whole structure; `type_option.strobe = 1` of a covergroup with a clocking event (`covergroup strobe sampling`), as no sample is taken in the Postponed region |
| **Sampling (19.3, 19.8.1)** | `sample()`; the clocking event of a covergroup (`covergroup cg @(posedge clk);`) for each instance: one that a declaration of a module or a program constructs with `new` as its initializer (`cg c = new;`, also in a generate block, a loop's included), and one that any other `new` constructs, in a procedure, a task, a function, the `new` method of the class of an embedded covergroup (19.4) or another declaration initializer of a module or a program (`C obj = new;`, `cg a[2] = '{new, new};`, a static property of a class). The event is any event control on signals, with edges, changes, `or` and comma lists and `iff` (9.4.2), on named events (also of an object: `@ev`, `@(this.ev)`, and `@(obj.ev)` where `ev` is a named event of the object a handle property `obj` holds), on properties of the class of an embedded covergroup (`@m_z`, `@(posedge clk_bit)`), on hierarchical names and on members of the interface instance a virtual interface variable or property denotes (`@(posedge vif.clk)`). It is bound where the covergroup is declared: in its scope, also a package or the compilation unit, or in the class of an embedded one. The instances of a covergroup of a module or a program, whatever constructs them, are sampled by the samplers of the type: at one `posedge` or `negedge` of a one-bit variable, with or without `iff`, with the flip-flops of the edge, before their nonblocking assignments; at every other event, and in a program, by processes that wait for it. An instance of an embedded covergroup, or of a covergroup of a package or the compilation unit, is sampled by a process its construction starts: see [Clocking events of covergroups](#deviations-standard-code) for how each differs from 19.3 | An instance with a clocking event that a declaration of a package or the compilation unit constructs, itself or through a subroutine or a class constructor it calls (`covergroup sampling event of an instance that a declaration of a package or the compilation unit constructs`); an instance of a covergroup embedded in a class, or of a covergroup of a package or the compilation unit, constructed in an `always_comb`, `always_latch` or `always_ff` procedure, a continuous assignment or a final procedure, itself or through a subroutine or constructor (`covergroup sampling event of an instance constructed in an always_comb procedure`, `… in an always_latch procedure`, `… in a continuous assignment`, `… in an always_ff procedure`, `… in a final procedure`), which would start a process there (an instance of a covergroup of a module or a program may be constructed in each); an event of an embedded covergroup on a value member of an object that a handle property holds, such as 19.4's `@(m_obj.m_ev)` with an `int m_ev`, also in `iff` (`covergroup sampling event on a member of an object that a handle property holds`), as no event control of a process supports one; every use of a covergroup with a clocking event that is declared in an interface, `sample()`, `get_coverage()` and the options included; a clocking event that names an argument of the covergroup (19.3: "A clocking event may refer to arguments of the covergroup"); clocking events under `Simulator` (see [Celox execution model](#celox-execution-model)); a block event (`@@(begin f)`); `sample()` in a sequence match item |
| **Arguments (19.3, 19.8.1)** | The arguments of a covergroup and of its `with function sample` declaration | `ref` arguments of a covergroup or of its sample method; arguments of unpacked type; an argument of the covergroup that a coverpoint reads and that is not of an integral type, a string or a class handle, such as a `real` |
| **`let` declarations (11.12)** | — | A `let` is not expanded in what follows the name of a covergroup: a covergroup whose argument types or defaults, coverpoint expressions, crossed variables, guards, bin values, `intersect` values, `with` expressions or option values name one is rejected, unless an argument of the covergroup or of `sample` has the name (`item` in a `with` expression is the value of the bin, whatever is named so) |
| **Methods (19.8)** | `sample()`, `get_inst_coverage()`, `get_coverage()`, `T::get_coverage()`, and `$get_coverage()`; `get_coverage` and `get_inst_coverage` with two `ref int` arguments; `start()`, `stop()` and `set_inst_name()`; the methods Table 19-5 gives coverpoints and crosses (`c.cp.get_inst_coverage()`, `c.x.stop()`, `T::cp::get_coverage()`); a method of an embedded covergroup called on its variable (`cg` in the class, `obj.cg`) | A method of an embedded covergroup called on another expression that gives the handle, such as a conditional expression |
| **Construction (19.3, 19.4)** | `new` with the arguments of the covergroup; in the `new` method of its class, an embedded covergroup assigned as `cg = new;` or `this.cg = new;` | `new` with a covergroup handle as a shallow copy (8.12); an embedded covergroup assigned through another handle of the class (`other.cg = new;`) in the `new` method (outside that method the assignment is the error 19.4 makes it) |
| **Type names (20.6.1)** | — | `$typename` of a covergroup type, of a variable of one, or of an array or structure that holds one (20.6.1 has no rule for the name) |
| **Results (19.9)** | `Simulation::coverage()` and `Simulator::coverage()` (see [Coverage](#coverage)) | The coverage database; see [No coverage database](#deviations-standard-code) |

### I/O, formatting, and system tasks {#formatted-output-to-strings}

Runtime I/O (`$fopen`, file `$display`/`$write`, `$system`, plusargs, `$sformatf` family, etc.) requires a timed `Simulation` process — calls from combinational processes and continuous assignments are rejected. `$system` is unsupported on WebAssembly hosts. For `$readmemh` and `$readmemb`, see [Memory files](#memory-files).

| Feature | Supported | Not supported |
| --- | --- | --- |
| **`$strobe` / `$fstrobe` (21.2.2, 21.3.2)** | Module/static expressions in scheduled processes (including `b`/`o`/`h` forms) | Calls in subroutines; procedural locals, unrolled loop variables, updates, or effectful calls in arguments |
| **`$monitor` / `$fmonitor` (21.2.3, 21.3.2)** | Whole static scalar variables, console `string` variables (`$monitor`), constants, and direct time queries in scheduled processes | Array elements, slices, general expressions, `string` variables in `$fmonitor`, and the `$strobe` call-context restrictions above |
| **`$fopen`, `$fdisplay`/`$fwrite`, `$writememh`/`$writememb`** | UTF-8 string literals, packed integral strings, and `string` variables for paths, modes, and formatted arguments; `$writememh`/`$writememb` on static unpacked arrays | Non-UTF-8 file names or modes; `$writememh`/`$writememb` on dynamic or associative arrays |
| **`$fgets` (21.3.4.2)** | Writable `string` variable or selected fixed string-array element destinations | Packed integral buffers, class properties, or dynamic containers as destinations |
| **`$ferror` (21.3.7)** | `string` variable, selected fixed string-array element, or packed buffer ≥ 640 bits | Class properties or dynamic containers as destinations |
| **`$sscanf` / `$fscanf` (21.3.4.3)** | Integral (`%d`, `%h`/`%x`, `%o`, `%b`, `%c`) and real (`%e`/`%f`/`%g`) conversions; string literals, packed integrals, and `string` variables as source and format | `%s` string outputs; `%t` time scaling |
| **`$sformat` / `$swrite` (21.3.3)** | `string` and packed integral destinations (`$sformatf` returning `string`) | Unpacked arrays of `byte` as destinations |
| **`%p` formatted output (21.2.1.6)** | Integrals, reals, strings, class handles, packed structs (without side-effecting calls), unpacked struct variables/members/class properties (without dynamic container members), enums, and containers (dynamic array, queue, associative) of those scalars/unpacked structs/enums in procedural bodies (integral elements ≤ 64 bits; integral, `string`, or scalar-enum associative keys) | Enum values returned by function calls (or calls with effects) or popped from queues; packed unions; packed structs returned by a function call or computed by a side-effecting call; packed arrays of enums; fixed-size unpacked arrays; structs with `real` or dynamic-container members; aggregate function results; containers with elements > 64 bits, `real` elements, other aggregates, or packed-enum/class-handle keys *(for run-time format `%p`, see [Deviations affecting standard-conforming code](#deviations-standard-code))* |
| **`$test$plusargs` / `$value$plusargs`** | Packed integral or string-literal queries; integer conversions into packed integrals, and `%s` into packed integrals or `string` variables | `real` conversions; `string` variable as the format/query argument; non-`%s` conversion into a `string` destination |
| **Time queries, `$timeformat`, and `%t`** | `$time`, `$stime`, `$realtime`, `$timeformat` (20.4.3; up to 4096 decimal places/width; see [Time and precision](#time-and-precision)), `%t` on numeric values; `$timeunit`, `$timeprecision`, `$printtimescale` on modules/instances | `%t` of a `string` value; `$timeunit`/`$timeprecision`/`$printtimescale` on selected instance array elements or generate paths; a scope time unit finer than the design's global precision |

## Narrow limitations in otherwise supported features {#narrow-limitations}

- **Design structure:**
  - Non-ANSI port expressions such as `module m({a, b}, c[3:0], .p(d))` ([#1953](https://github.com/celox-sim/celox/issues/1953)); plain non-ANSI ports (a header of port names with body declarations) are supported.
  - Non-constant ANSI `input` port defaults (23.2.2.4), defaults on non-`input` ports, and unpacked-structure port defaults; constant singular `input` port defaults are supported (constant unpacked-array `input` port defaults warn via `unpacked_port_default`, rejected in strict mode).
  - Implicit nets declared by port connections of an instance inside a generate block (6.10): an input connection reading one is rejected, and one driven only by outputs becomes a net of the module rather than of the block.
  - Instance arrays that are multidimensional, have a negative bound, or exceed 4,095 elements.
  - Package nets ([#1146](https://github.com/celox-sim/celox/issues/1146)); a package variable cannot be a clock or reset.
  - Wildcard imports (26.3): a module, interface, program, or package scope, a generate block, and a procedural block each bind a name once for their whole body (`export p::x;` binds it in the package), not per reference. References covered by one binding that denote different declarations at their positions are rejected ([#4646](https://github.com/celox-sim/celox/issues/4646)); a name that an earlier reference imported through a wildcard import stays bound to that declaration after a later wildcard import that also declares it, as the standard specifies. A block-local initializer reading a package variable through the block's own wildcard import is also rejected ([#4350](https://github.com/celox-sim/celox/issues/4350)).
  - Only one source file may declare `$unit` variables.
- **Types and expressions:**
  - Enum methods (`first`, `next`, …) on an element of a packed array of enums ([#2417](https://github.com/celox-sim/celox/issues/2417)).
  - Constant out-of-range part-select reads, and constant out-of-range part-select writes to an array element or an inner packed dimension ([#88](https://github.com/celox-sim/celox/issues/88)). Partial out-of-range constant writes to plain packed vectors (e.g., `q[9:6] = v` on `logic [7:0] q`, writing only `q[7:6]`) and all run-time selects follow the standard.
  - Run-time part-selects and element selects of a member of an element of a packed array of structures that is itself a member of a packed structure, such as `s.regs[i].q[j +: 4]`.
  - Reduction operators in parameter expressions ([#456](https://github.com/celox-sim/celox/issues/456)).
  - Type operator (6.23): comparing a type reference with a value, range items in a `case` on a type, and applying `type(...)` to an expression that reads a port or to any expression in a generate block.
  - `$typename` <a id="type-names"></a> (20.6.1): rejects types without one exact spelling instead of inventing system-generated names — anonymous enumerations, structs, and unions, soft and tagged unions, unpacked structures with unpacked array members, packed arrays of enumerations or of signed typedefs, classes with non-integral value parameters, and operands other than names and data types (such as `$typename(a + b)`). Virtual interface types (unspecified in 20.6.1) are written as declared (`virtual bus_if`, `virtual bus_if.mp`, `virtual bus_if#(8, 4)`).
  - Aggregate parameters as elaboration-time constants: unpacked array parameters hold their value from simulation start; an element of a 1D array parameter (and a select of its packed dimensions unless the element type is a packed array of a signed named type) is an elaboration-time constant only when elements are packed integrals ≤ 127 bits without X/Z bits and the array has ≤ 4,096 elements; a member of a packed-struct parameter is an elaboration-time constant unless wider than 127 bits or containing X/Z bits; whole unpacked arrays and other elements/members cannot be used where an elaboration-time constant is required.
- **Processes and subroutines:**
  - Nonblocking assignments inside `always_comb` ([#453](https://github.com/celox-sim/celox/issues/453)) and `always_latch` ([#431](https://github.com/celox-sim/celox/issues/431)); use blocking assignments there.
  - Package and `$unit` timed tasks with `static` arguments.
  - Constructing or extending the built-in `process` class; suspending a process's own thread or calling `await()` from a function.
  - Loops: procedural loops unroll at most 10,000 iterations; combinational loops with a run-time trip count must be counted `for` loops; loop-generate constructs within one module are rejected past 1,000,000 iterations in all; `genvar` updates using bitwise compound operators ([#455](https://github.com/celox-sim/celox/issues/455)) and recursive constant-function expansion in generate constructs are rejected.
  - `final` procedures <a id="final-procedures"></a>: timing controls, calls to user tasks (including through functions), and functions that schedule events after returning (13.4.4) are rejected; detached `fork … join_none` children declared in a `final` procedure are initialized but never start (see [Execution model](#celox-execution-model)).
  - Named subroutine arguments: calls whose selection indices have procedural effects, and method defaults using the receiver in an `always_comb` or `assign` call when the receiver or an earlier argument reading the object has effects.
  - `%m` (21.2.1.5) in a subroutine that runs in another instance's code (a hierarchical call, a call through a virtual interface or an interface port, an override of a class of a module) names the subroutine's own instance, except in two shapes where it names the calling instance: a subroutine imported through a port of an interface without data members, and an override of a class of a module that reads no item of the module ([#4642](https://github.com/celox-sim/celox/issues/4642)).

## Intentional deviations {#intentional-deviations}

### Deviations affecting standard-conforming code {#deviations-standard-code}

These accepted IEEE 1800-2023 constructs deliberately behave differently from the standard. Portable code should avoid relying on them:

- **Program continuous assignments do not run in the Reactive region.**
  Continuous assignments in a program are evaluated with the design's
  combinational logic, and design logic that a program writes settles while
  the reactive region set runs, so a program resuming after `#0` reads the
  new values. IEEE 1800-2023 4.5 evaluates them only once the reactive
  region set is empty. Icarus, Verilator, and xezim behave as Celox does.
- **Static storage of interface subroutines across modules and virtual interfaces.**
  A static function or task called through interface ports of several modules
  keeps its static variables, and the static formals of a task that suspends,
  once per calling module instead of once per interface instance (13.3.2,
  13.4.2; [#3739](https://github.com/celox-sim/celox/issues/3739)). Calls
  through the instance itself and through the port of a single module share
  one. Likewise, a task or function called through a virtual interface
  (`vif.drive(req)`, 25.9) holds static variables per calling scope, and
  formals belong to each call (even on static subroutines).
- **Procedural `assign` override timing on indirect writes.** While a
  procedural `assign` is active, a write to its variable other than by a
  plain assignment statement (an output argument, a concatenation target, a
  delayed nonblocking assignment, or a hierarchical name in another module)
  is overridden in the time step it happens rather than suppressed, so a
  process waiting on the variable can observe the written value.
- **Run-time `%p` format strings simplify non-string values.** When a `%p`
  conversion comes from a format string evaluated at run time rather than a
  compile-time literal, Celox prints `string` values in quotes and any other
  value as `%0d`.
- **No coverage database.** `$set_coverage_db_name` and `$load_coverage_db`
  evaluate their argument and report a compilation warning
  (`no_coverage_database` in
  [`[diagnostics]`](./celox-toml.md#diagnostics)); nothing is saved or
  loaded (19.9; [#4294](https://github.com/celox-sim/celox/issues/4294)).
  Read the coverage of covergroups with `Simulation::coverage()`. Immediate
  `cover` executes its pass statement; assertion coverage and the code
  coverage of clause 40 are not collected.
- **Clocking events of covergroups.** IEEE 1800-2023 19.3 samples a
  covergroup "the instant the clocking event takes place, as if the process
  triggering the event were to call the built-in sample() method". Celox
  samples the instances that declarations construct in one of two ways. An
  event that is one `posedge` or `negedge` of a one-bit variable, with or
  without `iff`, is an edge-sensitive procedure, `always @(posedge clk)
  c.sample();`, one for all the declarations of the type; it runs with the
  flip-flops of the edge and sees the values before their nonblocking
  assignments, also at a clock that a flip-flop divides or that a
  nonblocking assignment toggles. Every other event (`@(s)`, `@(edge s)`, a
  list such as `@(posedge a or negedge b)`, a named event, a member read
  through a virtual interface) and every event of a program is a process
  of its own that waits for it, `initial forever begin @(event);
  c.sample(); end`. An instance of such a covergroup that any other `new`
  constructs (`initial c = new;`, a task, a class method, `cg a[2] =
  '{new, new};`) joins a list of its type, which the same edge-sensitive
  procedure walks after the declarations' instances, or one process of the
  type at any other event (and one more for the list's fifth instance on):
  no process is started for it. An instance of a covergroup embedded in a
  class, or of a covergroup declared in a package or the compilation unit,
  is sampled by a process that its construction starts, as `fork forever
  begin @(event); c.sample(); end join_none` would: it draws no seed from
  the constructing process (18.14.1) and is no child of it, so `wait
  fork`, `disable fork` and `kill()` of that process do not see it. They
  differ from 19.3 as follows:
  - Several occurrences before the sampler runs again give one sample: a
    process that makes the event occur several times without suspending
    (`clk = 1; clk = 0; clk = 1;`, `-> e; -> e;`), and, for a process
    sampler, occurrences that several processes make in one time step
    before it runs (two processes raising the terms of `@(posedge a or
    posedge b)` at the same time).
  - The sample sees a blocking write that the triggering process makes
    after the triggering write (`clk = 1; d = 5;` samples `d` as 5).
  - A process sampler misses an occurrence before it first waits: one at
    time zero by an `initial` procedure that runs before it, or by the host
    before the first `run_until`, and for an instance whose construction
    starts a process every occurrence until the process that constructs it
    suspends. An edge sampler samples those, as the flip-flops of the edge
    run then. An instance in a list is sampled at an occurrence of the time
    step it is constructed in only if its sampler runs after the
    construction: one that an `always @(posedge clk)` constructs is first
    sampled at the next edge.
  - A process sampler at an edge of a variable that the NBA region updates
    (a divided clock, a clock toggled with `<=`) runs after the
    nonblocking assignments of that edge and sees their values, as a
    process the design writes with `initial forever @(...)` does
    ([#4771](https://github.com/celox-sim/celox/issues/4771)). So does the
    process that samples an instance of an embedded covergroup or of a
    covergroup of a package or the compilation unit.
  - An edge sampler beside an edge-sensitive procedure that calls
    `sample()` of the same covergroup type runs as a process, and so sees
    the nonblocking assignments of an edge the NBA region updates, as two
    such procedures the design writes do
    ([#4771](https://github.com/celox-sim/celox/issues/4771)).
  - When a virtual interface property that the event of an embedded
    covergroup reads (`@(posedge vif.clk)`) is assigned another interface
    instance, the wait that is armed already fires once more at the next
    edge of the old clock; the later ones are on the new one (9.4.2 waits on
    the new one at once). A task of the class that waits on the same event
    control does the same
    ([#4791](https://github.com/celox-sim/celox/issues/4791)); a virtual interface variable of a module is followed at once.
  - When a handle property whose named event the event names (`@(h.e)`)
    is assigned another object, the wait that is armed stays on the event
    of the object it was armed on, as a process of the source waiting on
    `@(h.e)` does and as 15.5.5.1 has it for a wait on an event variable
    that is assigned: until that event is triggered, the triggers of the
    new object's event are no samples.

  Occurrences separated by `#0` or in different delta cycles are each a
  sample, and a value that a nonblocking assignment of a blocking clock's
  edge updates is sampled before the update, as 19.3 has it.
- **`$exit` outside a program warns.** A `$exit` call from anywhere other
  than a program thread is ignored, as 24.7 specifies, and Celox also
  reports a compilation warning (`exit_outside_program` in
  [`[diagnostics]`](./celox-toml.md#diagnostics)), including for calls in
  unused function declarations.
- **UVM DPI functions are built in.** Imports with the C names of UVM's
  `src/dpi` functions bind to Celox's implementations instead of a user C
  function; see [DPI-C imports](#dpi-c-imports).
- **Opt-in compatibility modes.** Options in
  [`[compatibility]`](./celox-toml.md#compatibility) (such as
  `legacy_empty_reductions` and `shared_always_temporary`) preserve standard
  IEEE 1800-2023 semantics by default and deviate only when explicitly enabled.

### Permissive acceptance of non-standard code {#deviations-nonstandard-code}

Celox intentionally accepts the following non-standard constructs and compatibility extensions. Portable code should avoid relying on them.

**Warn by default, rejected in strict mode (`strict = true` / `--strict` / `.strict(true)`):**

- **A missing `static` keyword on initialized locals (6.21).** IEEE
  1800-2023 6.21 requires an explicit `static` when a static variable's
  declaration specifies an initializer. Celox also accepts the declaration
  without `static` with a constant initializer in any procedure, and with a
  run-time initializer in a procedure that runs once (such as `initial`),
  reporting an `implicit_static_initializer` warning (`warn` by default,
  `error` in strict mode). In an `always` procedure, a run-time initializer
  without `static` or `automatic` is rejected.
- **Semicolons after an `` `include `` filename (22.4).** IEEE 1800-2023 22.4
  allows only white space or a comment on the line of an `` `include ``
  directive after its filename. Celox ignores semicolons there
  (`` `include "uvm_macros.svh"; ``), as other simulators do, and reports an
  `include_trailing_semicolon` warning (`warn` by default, `error` in strict
  mode). Any other text after the filename is rejected.
- **Default time scales when explicit time scales are mixed (3.14.2.3).**
  Instead of rejecting designs where only some elements specify or inherit a
  time scale, Celox reports a `mixed_timescale` warning (`error` in strict
  mode) naming those elements and assigns them 1 ns / 1 ps, as other
  simulators do.
- **Other warn-by-default compatibility diagnostics.** Duplicate module
  declarations (`duplicate_module`), `$size` on fixed-size interface arrays
  (`interface_array_query`), `bus.WIDTH` parameter shorthand on interface
  arrays (`interface_array_parameter`), constant unpacked-array `input` port
  defaults (`unpacked_port_default`, 23.2.2.4), and memories shared between
  `always_ff` and another edge process (`multiple_always_ff_drivers`) warn by
  default and are rejected in strict mode; see
  [`[diagnostics]`](./celox-toml.md#diagnostics) (alongside `inferred_latch`).

- **Packed values and unbased fills assigned to fixed-size unpacked arrays
  (7.6).** With `packed_to_unpacked_assignment` at `warn` (the default) or
  `allow`, Celox copies an exact-width packed value bit for bit or fills every
  element with an unbased unsized literal such as `'0`. Strict mode rejects
  these assignments and equivalent module port connections (10.8) by
  default. Use an explicit cast to the array type or an assignment pattern
  such as `'{default: '0}`.
- **Procedural `force` and `release` on variable selects and unpacked array
  elements (10.6.2).** `nonstandard_force_target` warns by default and becomes
  an error in strict mode for constant bit/part-selects or members of variables
  and constant-indexed elements (or selections of those elements) of fixed-size
  unpacked arrays of packed integrals. Forcing an array element can also emit
  the `forced_array` diagnostic about the size of the force controls.
- **Vendor alias `$psprintf` (21.3.3).** Celox interprets it as `$sformatf`
  and emits `psprintf_alias` by default. Strict mode rejects the alias by
  default.

## Implementation-defined and unspecified behavior {#semantics-worth-knowing}

Where IEEE 1800-2023 leaves a choice to the implementation, or declares an
order unspecified or nondeterministic, Celox chooses as follows.
Portable code must not depend on these choices.

### Constant widths {#constant-widths}

IEEE 1800-2023 lets an implementation limit the width of a vector to no
less than 65,536 bits (6.9.1, 7.4.1). A constant value, such as a parameter,
the concatenation that gives it or a part-select of it, may have up to
1,048,576 bits. A replication, a constant built bit by bit, and a value inside
a constant function call may have up to 65,536 bits. A wider constant is
rejected before it is built.

### Event scheduling {#timing-semantics}

Clause 4.7 permits either order in the cases below; Celox is deterministic:

- Continuous assignments, combinational processes, and event expressions
  see a timed process's writes when that process reaches a timing control
  or ends. After `b = 1; a = 1;`, `a ^ b` has not changed, and a pulse
  `a = 1; a = 0;` wakes `@(a)` but not `@(a ^ b)`; write `#0` between the
  writes to make the pulse visible to an expression wait. Compound waits
  on fork-shared automatic locals instead observe each supported blocking
  assignment and statement-call copy-out.
- A waiter sees every value one evaluation of `always_comb`, `always @*`,
  or an edge-triggered process writes, including one a later write
  overwrites (4.3, 9.4.2). Through a child instance's output port — a
  continuous assignment (23.3.3) — a waiter in the parent sees only the
  settled value.
- An `expr iff condition` item (9.4.2.3) is judged at each write of `expr`
  when both are variables, so in one process `clk = 1; en = 0;` is an event
  and `en = 0; clk = 1;` is none. When `expr` or the condition is a general
  expression or a net, or `expr` is written by a flip-flop domain
  ([#1830](https://github.com/celox-sim/celox/issues/1830)), the condition
  is judged when the occurrence is detected, after combinational logic
  settles.
- A `wait` condition reading class properties is re-checked after any store
  of a property it reads — through any handle and after every `randomize()`
  call — including stores of the same property of another object or of the
  value already held, so it may be checked more often than the minimum
  (9.4.3).
- With an asynchronous reset, a `$past` history tick is a run in which the
  clock has left the level it had before the time slot; a clock that
  returns to that level within the slot does not move the history.
  Multiple clocking-event transitions within one time step are undefined in
  concurrent assertions (16.5).

### Time and precision {#time-and-precision}

- The implementation default time scale is 1 ns / 1 ps. A `Simulation`
  counts time in the design's global time precision (3.14.3), 1 ps by
  default, so `%t` reports picoseconds in a design without time scales.
- `$timeformat` (20.4.3) sets the units, decimal places, suffix and
  minimum field width of the `%t` conversions after it, in every output and
  in `$sformatf`; the time rounds half away from zero, and the width of a
  conversion such as `%0t` replaces the minimum field width. The suffix is
  a string literal, NUL characters included. An `always_comb`,
  `always_latch` or `always @*` that calls it, also through a subroutine,
  runs as a process: once at time zero (except `always @*`), then after
  each change of what it reads. A units number outside 2 to -15 or a negative
  precision or field width computed at run time ends the simulation with an
  error, as a constant one fails the analysis; Celox prints at most 4,096
  decimal places and pads to at most 4,096 characters, and a larger
  precision or field width is an error.
- A class or subroutine declared in the compilation unit (`$unit`) uses
  the compilation unit's time unit and precision. A `` `timescale ``
  directive does not set those (3.14.2.3), so they are the default 1 ns /
  1 ps unless `$unit` declares `timeunit`/`timeprecision`. Verilator
  applies the last `` `timescale `` instead. For portable code, declare
  `timeunit` in `$unit`. A precision finer than the global precision
  rounds to whole simulation steps.
- `final` procedures run sequentially in a deterministic but unspecified
  order. Relative order of package and module variable initializers that
  call constructors or functions is likewise unspecified (6.8, 10.5); do
  not rely on it.

### Random numbers {#random-numbers}

- `$urandom` uses a deterministic 32-bit linear congruential generator
  (18.13.1, 18.14); its default seed and sequence are
  implementation-defined and need not match other simulators. `$random`
  and the `$dist_*` functions follow Annex N, so their sequences match
  other implementations of that annex. The random-state strings of
  `process` and of class objects (`get_randstate`, 18.13.4) are opaque and
  specific to Celox; `set_randstate` (18.13.5) accepts only a string that
  `get_randstate` returned and otherwise ends the simulation with an error.
- In combinational procedures, random draws occur when the process
  activates; repeated evaluation with unchanged inputs does not advance
  the stream.
- Constrained `randomize()` searches for a satisfying assignment with the
  object's seed and updates all selected properties together; the search
  does not guarantee a uniform distribution
  ([#2187](https://github.com/celox-sim/celox/issues/2187)). Proved
  infeasibility returns `0`, preserves the properties, and skips the
  post-hook (18.6.3); resource exhaustion is a runtime error.
- Soft constraints (18.5.13) are tried from the highest priority down
  (18.5.13.1): each is kept when the hard constraints and the soft ones
  kept before it allow it, and is otherwise discarded; a `disable soft`
  discards the lower soft constraints that name its variable (18.5.13.2).
  The search for each keeps the seed, so the choice is reproducible. A
  soft `dist` constrains its operand to its items of nonzero weight; while
  it is kept, its weights shape the choice as those of a hard one do
  (18.5.3), and a discarded one has no effect on it.
- A 4-state `rand` property takes a 2-state value. The X and Z bits of a
  4-state state property, of the value of a function a constraint calls
  whose type is 4-state, and of a 4-state caller-scope input of inline
  constraints or of `std::randomize() with`, follow the 4-state semantics
  of each operation (11.4): a constraint or guard whose truth they decide
  ends the simulation with an error (18.3), one they do not decide, as
  through a mask, a select or an `&&` a known 0 decides, takes its value,
  and such a guard is evaluated before solving (18.5.13). Where the truth
  also depends on random variables, the solution found decides whether
  they reach it.
- `solve before` (18.5.9) leaves the solutions as they are, and the search
  does not order its choices by it; a circular order is not diagnosed.
- A function a constraint calls (18.5.11) runs as any call of it does,
  the override of the object's class included, after the random variables
  in its arguments are solved by the constraints that name only them; its
  value is state to the other constraints, so they can leave the call no
  solution, and a random variable it reads that is no argument has its
  value before the call.
- `randsequence` (18.17): a production of several rules picks one by their
  weights, 1 unless given, from the thread's random number generator
  (18.14.2). `rand join` interleaves its productions' items at random from
  the same generator; 18.17.5 gives the bias no formula, so Celox evaluates
  it once and, at each step, with probability |2 × bias − 1| advances a
  production with the fewest items left (bias below 0.5) or the most (above
  0.5), and otherwise any production with items left, each as likely (0.5
  prefers no length; 0.0 and 1.0 always prefer the shortest and longest).
- Array `shuffle` draws from the calling process's random stream.

### Coverage {#coverage}

`Simulation::coverage()` and `Simulator::coverage()` report every covergroup
type that has an instance, with its instances, their numbers of samples and
the coverage `$get_coverage` gives. Reading the report changes nothing and
advances no time.

- **A covergroup type belongs to its elaborated scope.** Instances of a
  module with the same parameter values share the covergroup types the
  module declares, as they share its typedefs; 6.22 makes them types of each
  instance. Elaborations with other parameter values declare types of their
  own, which the report lists under one name. A covergroup of a package is
  one type of the design, and one of a compilation unit a type of its
  source file.
- **`sample()` on `null` samples nothing**, for an embedded covergroup that
  the constructor did not create (19.4) and for any other covergroup
  variable. `get_coverage()` and `type_option` through a handle are those of
  the type and do not read the handle; `get_inst_coverage()`, `start()`,
  `stop()`, `set_inst_name()` and `option` on `null` are a runtime error.
  The expression that names the covergroup of an operation of the type is
  not evaluated either: in `f().get_coverage()` or `f().type_option.weight`
  `f` is not called, as a static member of a class is read without its
  object (8.9, 19.7.1).
- **Options assigned in a procedure** take the value converted to the type
  19.10 gives the member, as a variable of that type takes it (a `real` is
  rounded, x and z bits are 0), and count from then on: the hits an
  instance has are those it had, and `at_least`, `weight` or
  `merge_instances` change only how they are counted. `option.at_least` of a
  covergroup is that of each of its coverpoints and crosses that does not
  assign its own, also when a procedure assigns it after the item read it
  (Table 19-2); `weight`, `goal` and `comment` of the covergroup are not
  those of its items (19.7). `goal` changes no number and is shown in the
  report. A negative weight ends the simulation with an error ("shall be a
  non-negative integral value", Table 19-1). `set_inst_name(s)` assigns
  `option.name`, which is `<type>#<n>` until it is assigned. The options of
  the instances and of the types are part of a checkpoint.
- **`start()` and `stop()`** set whether the covergroup and each of its
  coverpoints and crosses collect, each on its own; an instance starts
  collecting. An item counts a sample where it and the covergroup collect: a
  stopped covergroup evaluates nothing of a sample, and a stopped coverpoint
  leaves its crosses without the sample, as one whose guard is false does,
  and reports no hit of its illegal bins.
- **With two arguments** `get_inst_coverage` gives the covered bins and the
  bins of the item, or of all items of the instance, and `get_coverage`
  their sums over the instances (19.8); with `type_option.merge_instances`
  `get_coverage` gives those of the union of the bins of the instances
  (19.11.3). Without `merge_instances`, these sums count the bins of every
  instance, also of one whose `option.weight` is 0, which the coverage the
  method returns leaves out (19.11.3). With `type_option.merge_instances` and
  without
  `option.get_inst_coverage`, `get_inst_coverage` gives what `get_coverage`
  does (Table 19-1); without `merge_instances` it is always the coverage of
  the instance.
- **`option.detect_overlap`** warns of each pair of overlapping bins of a
  coverpoint with a `$warning` report of the `new` that constructs the
  instance (Table 19-1).
- **`type_option.strobe`** has no effect on a covergroup without a clocking
  event: it "only applies to the scheduling of samples triggered by a
  clocking event" (19.3). A nonzero value on one with a clocking event is
  rejected (`covergroup strobe sampling`).
- **The instance is sampled, not the variable.** A
  declaration `cg c = new;` of a covergroup with a clocking event samples
  the instance it constructs at each occurrence of the event, whatever `c`
  holds later: `c = null` or `c = d` neither stops that nor samples another
  instance twice, and the instance is never freed. So is an instance that
  any other `new` constructs, from its construction on: assigning
  `c = new;` again samples both instances. An embedded covergroup is
  sampled until the property of its object holds another instance, which
  the `new` method of its class constructs there (19.4 allows the
  assignment in that method alone): the former instance counts no sample
  of the event from the next occurrence on. The sampler holds the instance,
  and the object of an embedded one, for the rest of the simulation. `sample()` and the other
  methods of 19.8 apply to such an instance as to any ("These methods can be
  invoked procedurally at any time"): `sample()` samples it once more, and a
  stopped instance counts no sample of its event. No sample is taken before
  the first occurrence of the event, also of a change event (`@(s)`) in
  two-state simulation. In four-state simulation an assignment at time
  zero that makes a variable 0 or 1 is an edge (Table 9-2) that an edge
  sampler samples, as the flip-flops of the edge run then; a declaration
  initializer is none (10.5).
- **A program samples in the Reactive region set.** The instance a
  declaration of a program constructs is sampled after the nonblocking
  assignments of the design at the same event, as every process of a
  program is scheduled (24.3.1): at a rising edge it sees the values the
  flip-flops of the edge assigned. 19.3 can be read to sample where the event
  occurs instead.
- **A covergroup without items has no coverage.** `get_inst_coverage()` and
  `get_coverage()` of a covergroup that declares no coverpoint or cross are
  0, and it takes no part in `$get_coverage()`, which is 100 when no
  covergroup of the design does (19.11).
- **Bin values are compared as values of the coverpoint type (19.5.7).** A
  value the type does not have takes no part in its bin, and a range takes
  part with the values the type has. For a constant the build warns
  (`covergroup_bin_value`); for a value that an argument of the covergroup
  gives, the `new` that constructs the instance issues a `$warning` report
  with every such value of the instance, and one for
  `option.auto_bin_max` below 1, which makes one automatic bin. A range
  whose bounds differ in signedness is read in their common type, as the
  operands of a binary operator are (11.8.1): in `[-1:8'd5]` the bound -1
  is a large unsigned value, which is warned of too. An unsigned value on a
  signed coverpoint stands for the value its bits read as: `8'hFF` is -1 on
  an 8-bit signed coverpoint.
- **x and z.** A sample with an x or z bit is in no automatic bin (19.5.3).
  On a coverpoint of a 4-state type, a bin value written with x or z bits
  holds exactly the samples with those bits; on one of a 2-state type it
  takes no part, with a warning (19.5.4 and 19.5.7 differ here). A bound of
  a range with such bits drops the range. In two-state simulation no
  sample has such a bit.
- **An illegal bin hit is an error report.** A sample of a value of an
  `illegal_bins` bin whose guard holds issues a report of `$error` severity
  that names the bin, its coverpoint, its covergroup, the instance and the
  value (19.5.6); a sample that hits several such bins reports them in one.
  The simulation goes on, the sample counts in no `bins` bin, and the run
  ends as any run with an error report does. The values of an illegal bin
  are values of no other bin whether or not its guard holds, and so are
  those of `ignore_bins`: a guard on these gates the hit count of the bin
  itself, which the report shows, and that of an illegal bin its error.
- **Bins are made when an instance is constructed** (19.5.1): a bin for
  each value (`b[]`) in the order the values first appear, named
  `b[value]`; `b[N]` as 19.5.1 distributes the values, named `b[0]` to
  `b[N-1]`. The values of `ignore_bins` and `illegal_bins` are taken once
  the values are distributed, and a bin that is left without a value takes
  no part in the coverage and is not listed (19.5.5, 19.11.1). A number of
  bins `N` that is not positive is an error of the build for a constant
  and ends the simulation with an error for an argument.
- **`default` bins** count the samples that are in no bin of a declaration
  of their coverpoint, `ignore_bins` and `illegal_bins` among those: an
  ignored value is no `default` value. A sample with x or z bits that no
  bin lists with those bits is such a sample, so `illegal_bins b = default`
  reports it in four-state simulation. A `default[]` array has a bin for
  each of the first 4,096 values that are sampled, named with the value (a
  value with x or z bits as a sized binary literal, `b[4'b1x0z]`); the
  samples of further values are counted in `omitted_bins` of the report.
  A coverpoint with a `default` bin of `bins` has no automatic bins, and
  where it declares no other `bins` it has no bin to cover (19.11.1).
- **Wildcard bins** (19.5.4) list the values their x, z and `?` bits stand
  for, so that ignored and illegal values are taken from them as from any
  bin. A value of more than 65,536 ranges of values (`4'b11??` is one
  range, `4'b1??1` four) is instead matched when it is sampled: what is
  counted is the same, but `ignore_bins` and `illegal_bins` take no value
  from such a bin,
  and as a value of `ignore_bins` or `illegal_bins` it leaves no bin
  without a value; the `new` of the instance warns where that can matter.
  Only one bin without a `with` filter may have such a value. A range of a
  wildcard bin whose bounds have x or z bits is the values from its lowest
  low bound to its highest high bound.
- **`with` filters** (19.5.1.1) are asked about each value of their bin
  when an instance is constructed, at most 1,048,576 values; a bin whose
  values depend on an argument and are more ends the simulation with an
  error. Before `with`, the name of a coverpoint of an enumeration stands
  for its named values, as its automatic bins do (19.5.3). What the
  expression reads beside `item` and constants, such as an argument of the
  covergroup, a string among them, or a constant of the object of an
  embedded covergroup, decides the bins with the values of the bin: an
  instance that is constructed with the values an earlier one of its type
  was constructed with takes its bins without evaluating the expression
  again (19.5.1.1 allows caching). A filter that reads an object, whose
  state those values do not tell, is evaluated by every construction: one
  that calls a method of the class of an embedded covergroup that is not
  static, which may read the constants of its object (19.5), and one with
  a handle among its operands, such as `h.ok(item)` or `f(h, item)`. Any
  other function is taken to read its arguments and global constants only,
  as 19.5 requires. The filter of one `bins` bin (no array) whose values are constants and more
  than that, such as `bins b = cp with (...)` on a coverpoint of more than
  20 bits, is evaluated for the sampled value at each sample instead. Such
  a bin always counts as a bin with values, also when the expression keeps
  none, and `ignore_bins` and `illegal_bins` take from it without regard to
  the expression.
- **Set expressions** (19.5.1.2). The elements of an array parameter
  (`localparam int S [3] = '{1, 4, 6};`, also of a package) are the values
  of the bin as if it listed them, and the build warns of those the
  coverpoint type has not. The elements of a dynamic array or a queue,
  such as a function returns for the arguments of the covergroup, and of a
  constant array of the class of an embedded covergroup are read when an
  instance is constructed, which then warns. The first element is the
  first value: `bins b[2] = S` of four elements has the first two in
  `b[0]`.
- **Tolerance ranges** (11.4.13) take integral operands; the tolerance of
  `[A +%- B]` is `A * B / 100` as integers divide, towards zero, which is
  what the standard's conversion of `A*B/100.0` gives for the values a
  `real` holds exactly.
- **Automatic bins of a signed coverpoint start at its lowest value**, and
  are named by their values: `auto[3]`, `auto[0:3]`, `auto[NAME]` for an
  enumerator. A coverpoint without a label has the name of its variable
  where its expression is that name alone (19.5). Of any other expression,
  a member of an object (`t.kind`) and a select of a variable among them,
  it is shown as `$point<n>`, with the number of the coverpoint in the
  covergroup from 1.
- **Crosses** (19.6). The coverpoint of a variable that a cross crosses
  and that is no coverpoint of the covergroup is named by the variable and
  follows the coverpoints the covergroup declares, one for each variable
  however many crosses cross it; it takes part in the coverage of the
  covergroup as they do. A cross without a label is shown as `$cross<n>`,
  with the number of the cross in the covergroup from 1. An automatic bin
  of a cross is named by its bins, `<a1,auto[3]>`. The bins of a cross are
  those of its coverpoints that take part in their coverage: a `default`,
  ignored or illegal bin and one left without a value have no cross
  products (19.6, 19.11.1).
- **A cross is sampled where each of its coverpoints counts the sample**
  in such a bin (19.6.1: "every coverage point in the cross matches its
  corresponding bin"). A coverpoint whose guard is false, or whose value is
  in no bin of it that takes part, or only in bins whose guard is false,
  leaves its crosses without the sample. A sample in two bins of a
  coverpoint is two cross products of its crosses. A cross bin counts a
  sample once however many of its cross products it is, and a cross bin
  whose guard is false counts nothing; its cross products stay those of no
  automatic bin.
- **Cross bins.** The values of an `intersect` list are resolved as those
  of a bin of the coverpoint of its `binsof` (19.5.7), with the warnings of
  bin values: of the build for a constant, of the `new` of the instance for
  a value an argument gives. `intersect` keeps the bins that hold one of
  them after ignored and illegal values are taken from the bins. The cross
  products of `ignore_bins` and `illegal_bins` are those of no other bin
  (19.6.2, 19.6.3), and a declared `bins` bin left without a cross product
  takes no part in the coverage. A sample of an illegal cross product
  issues a report of `$error` severity that names the bin, the cross, the
  covergroup, the instance and the cross product, as an illegal value of a
  coverpoint does. With `option.cross_retain_auto_bins` 0 a cross that
  declares any bin, `ignore_bins` and `illegal_bins` among them, has no
  automatic bin (Table 19-1), so a cross with only `ignore_bins` has no bin
  and takes no part in the coverage (19.11.2).
- **Crosses are not listed product by product.** A report lists the bins
  a cross declares, its automatic bins that were sampled, then as many
  others as `option.cross_num_print_missing` asks for, at most 4,096; the
  number of the others is in `omitted_bins`. A construction ends the
  simulation with an error where the bins of a cross tell more than
  1,048,576 classes of cross products apart, where a cross has more than
  2^128 cross products, and where sorting the cross products into its
  bins takes more than 2^26 steps; with `type_option.merge_instances`, a
  query of the coverage of the type does where uniting the automatic bins
  of its instances takes more than 2^26 steps (the report counts them
  however long it takes). An `intersect` list that meets values of a bin
  that only a sample sorts, those of an ignore or illegal wildcard value of
  more than 65,536 ranges or of a bin whose `with` filter is evaluated at
  each sample, is not supported: the construction ends the simulation with
  an error that says so.
- **A coverpoint has at most 1,048,576 bins.** A construction that would
  make more ends the simulation with an error, as does a sample whose
  values the heap cannot take as those of the coverpoints of its type,
  which only an error of Celox causes; such a sample is not counted.
- **Sampling calls a function of a coverpoint once.** The guards and the
  expressions of all coverpoints of a `sample()` are evaluated in the order
  of their declaration before any of them is counted, so a function among
  them may itself call `sample()`.
- **Coverage is cumulative and instances are never freed.** The samples of
  an instance stay in the coverage of its type and in the report when
  nothing refers to it any more; the report marks it as no longer live. A
  testbench that constructs a covergroup for each transaction grows the
  report by an instance each time.
- **Constructing a covergroup draws no random number**, so adding one to a
  procedure or to a class constructor leaves its random stimulus unchanged
  (18.14.1), also one with a clocking event, whose sampler takes no seed. A covergroup variable declared with an initializer in a
  module is a process of its own, as a class variable with one is, and
  takes a place in the order in which processes are seeded.
- **Coverage is part of a checkpoint and not of a state file.**
  `Simulation::restore` restores the coverage of the checkpoint;
  `save_state` and `load_state` fail with `StateError::Coverage` once a
  covergroup was constructed.
- Instances are named `<type>#<n>` in the report, numbered from 0 in the
  order of their construction within the type.

### Host-dependent results

- `$ferror` codes and wording are host dependent; Celox also exposes a
  failed `$fopen`'s details through FD 0.
- `$system` returns the C `system()` value, including the host platform's
  exit-status encoding (20.17.1). Relative file paths use the host
  process's working directory.
- `$fgets` with no bytes read leaves the destination unchanged; 21.3.4.2
  specifies the returned count but not that destination value.

## Celox execution model {#celox-execution-model}

These choices belong to Celox's API, not to SystemVerilog; they decide
which designs need the timed `Simulation` API and what the host can observe.

- **`Simulator` runs no processes.** Constant initialization applies to
  both APIs. `final` procedures, `initial` blocks that read design state or
  call system tasks, processes with timing controls, and `always` blocks
  whose sensitivity list is neither `@*` nor exactly the variables the
  statement reads run as processes and need `Simulation`. Building a
  `Simulator` for such a design fails with
  `SimulatorErrorKind::ProcessRequiresSimulation`, naming the first process
  and its source location. An `always` whose list (or part-selects) covers
  exactly what the statement reads, with no timing control, nonblocking
  assignment, subroutine or system task call, self-read, or repeated store,
  simulates like `always @*` in both APIs; so does a flip-flop-shaped
  edge-sensitive `always`. `Simulator::tick` drives the rising edge of its
  event only; a `Simulation` runs every edge. A `Simulator` call is one
  time slot for `$sampled` and deferred assertions, and `$time` returns 0.
- **Covergroups.** A covergroup variable declared with an initializer in a
  module or program (`cg c = new;`) is constructed by a process at time
  zero, which needs `Simulation`: building a `Simulator` for such a design
  fails as for any process. Under `Simulator`, construct the covergroup in
  a clocked procedure (`always @(posedge clk) begin if (c == null) c = new;
  c.sample(); end`). A covergroup variable of a module is a handle in the
  state of the design, as a class handle is: the compiled scheduler of a
  `Simulation` then calls into the host at the end of each time slot, or
  returns to it there where it has no such call (it has one on x86-64),
  also when nothing constructs the variable. Under `Simulation`, sampling
  from clocked logic adds only the calls that sample. Under `Simulator`, a
  clock whose clocked procedures construct or sample a covergroup also
  loses the `tick` that is one run of its clock domain: as any call into
  the host from clocked logic, the call is taken for one that may read the
  clock, so each `tick` moves the clock to its active level and back, which
  each tick pays beside the calls that sample. `sample()` from
  `always_comb` or `always @*` samples at each evaluation of the procedure
  and makes the design evaluate its whole combinational logic at each
  settle instead of the part a change reaches. Earlier releases ignored
  `cg c = new;`, so such a design built as a `Simulator` and had no handle
  in its state. An instance sampled at the clocking event of its type (`cg
  c = new;` of a `covergroup cg @(posedge clk);`) needs `Simulation`, as
  its declaration is a process. At one edge of a one-bit variable it is
  sampled by an edge-sensitive procedure, which costs what `always
  @(posedge clk) c.sample();` costs, one for all the declarations of the
  type; at any other event by a process of each declaration, which costs
  what `initial forever begin @(event); c.sample(); end` costs, a kernel of
  the compiled scheduler (one that waits for a named event, or one of a
  program, runs in the runtime's loop). Each declaration adds the hidden
  handle variable that holds the instance, `%covergroup_site<n>`, which
  the signal listings and waveforms show, as they show the hidden variables
  of delayed continuous assignments. An instance of such a type that any
  other `new` constructs needs `Simulation` too and costs about what an
  instance of a declaration costs: the samplers of the type walk the list
  of the type, four hidden handle variables (`%covergroup_instance<n>_<k>`),
  a count (`%covergroup_instance_count<n>`) and a queue
  (`%covergroup_instances<n>`) for the fifth instance on, whose elements
  each sample reads with calls into the host. The list holds its instances
  for the rest of the simulation. An instance of a covergroup embedded in
  a class, or of a covergroup of a package or the compilation unit, is
  sampled by a process with activation frames of its own, which runs in
  the runtime's loop, as `fork forever begin @(event); c.sample(); end
  join_none` written in a function does: while one lives, every time slot
  of the design runs in that loop instead of the compiled scheduler. For a
  type of a package or the compilation unit this holds for every instance,
  a module's declaration `p::cg c = new;` included. That
  process holds the instance for the rest of the simulation: an object
  whose class embeds a covergroup with a clocking event is never collected
  once its constructor ran. A covergroup with a clocking event
  that nothing constructs costs nothing. Covergroups are not available in the
  browser (WASI) build, which has no heap.
- **Host clocks.** `Simulation::add_clock` and `Simulation::schedule` move
  an event signal: a clock or asynchronous reset of an edge-sensitive
  `always`, or a one-bit input of a top-level module that a process waits
  for. A process waits for the input its event control names
  (`@(posedge clk)`, `@(clk)`) or its `wait` condition reads: in the top
  module itself, by a hierarchical name, or through port connections and
  continuous assignments that copy the whole signal unchanged. For any other
  signal `schedule` returns `NotAnEvent` and `add_clock` does nothing: an
  input read only as data, an `iff` condition, a multi-bit input, and the
  operands of an event expression such as `@(posedge (clk & en))` are driven
  with `modify` between the runs. With several top-level modules,
  `named_events()` names the events of each top from it (`hdl_top.clk`), and
  `add_clock_by_id` and `schedule_by_id` move them. An event signal cannot
  be forced from the host: `force_signals` rejects a waited input as it
  rejects a clock. A clock on a waited input that another event is computed
  from (a gated clock, an inverted reset) is not compiled into the
  scheduler's time slots, so each of its edges is a step of the host.
- **Termination.** `final` procedures run once on `$finish`, `$fatal`, or an
  explicit `Simulation::finish()`; an idle `step()` or the end of
  `run_until()` does not terminate the reusable Rust simulation. No
  scheduled events run afterwards, so a nonblocking assignment made
  directly in a `final` procedure never applies its queued update, and
  detached `fork … join_none` children declared in a `final` procedure are
  initialized but never start. `$stop` in an `initial` block suspends the
  simulation (`is_stopped()` is set and the next call continues); in an
  `always` process it is reported as `RuntimeEvent::Stop`, as `$finish` is
  `RuntimeEvent::Finish`. Batch hosts without an interactive mode, such as
  the VPI host, end the run at `$stop`. A checkpoint cannot be taken while
  suspended (`CheckpointError::Stopped`).
- **Reports.** `$info`, `$warning`, `$error`, and `$fatal` — including the
  default `$error` of an assertion without a fail action — are delivered to
  the host as `RuntimeEvent::Report` with severity, message, file, line,
  scope, and simulation time. Only `$error` and `$fatal` fail a `celox`
  run; `$fatal` also ends the simulation.
- **Top-level modules.** A simulation runs the top module it names. A design
  with several top-level modules (23.3.1), such as a testbench's `hdl_top` and
  `hvl_top`, runs them together when Rust's `SimulatorBuilder::tops` or a
  repeated `celox run --top` names them, each as an instance of its own name
  under `$root`, with its ports unconnected. Hierarchical names, `%m`,
  waveform scopes and the VPI and reflection hierarchy then start at a top
  (`hdl_top.dut`, `$root.hdl_top.dut`): each top is a root scope, as
  `vpi_iterate(vpiModule, NULL)` returns them. Signals are named from a top,
  as `signal("hdl_top.clk")` or `child_signal(&[("hdl_top", 0)], "clk")`,
  and so are forced and live signals. The design's tops are the selected
  modules: `PreserveTopPorts` keeps the ports of each, `named_signals()`
  names each top's signals from it (`hdl_top.clk`), `named_hierarchies()`
  gives each top's hierarchy, and `named_hierarchy()` is `$root` with the
  tops as its children. Several tops require SystemVerilog sources only and
  take no parameter overrides.
- **Observable names.** Interfaces are expanded into the modules that use
  them: a member `m` of interface instance `h` becomes the signal `h$m`, an
  interface port at the top becomes ports such as `bus$data`, and a module
  with a generic `interface` port is copied per connected interface as
  `M$I`. Instance array elements are addressed by their declared index
  (`dut.u[3]`, `child_signal(&[("u", 3)], "y")`). A package variable is
  named `p::v` and a compilation-unit variable `$unit::v` in
  `sim.signal(…)`. Hierarchical names can address signals, including
  `string` variables, in both SystemVerilog code and the host API.
- **Heap and checkpoints.** Strings, class objects, containers, and the
  objects `process::self()` returns live in a heap collected between time
  slots and after each `tick`; a `string`, class handle, `process` handle,
  or virtual interface is stored like a 32-bit integer. A process that
  ended keeps its status for as long as a `process` handle names it, and
  its storage is reused once the heap has collected every such handle.
  Core checkpoints (`checkpoint()` / `restore()`) preserve
  heap objects, fork children, process handles, random streams, pending
  strobe/monitor state, and deferred assertion queues. Portable state
  files are narrower: saving fails while any variable holds a class
  object, semaphore, mailbox, or exposed `process` handle, while a process
  waits for a named event or a current-slot trigger is still visible to
  `triggered`, while strobe output is pending, and for designs with file
  I/O, source VCD controls, or monitor tasks. Non-null class objects or
  dynamic containers, and nonempty string/object values in pending NBA
  writes, host writes, or event-wait baselines, likewise require a core
  checkpoint. Native images and serialized checkpoints from earlier heap
  or container layouts must be regenerated.
- **Loop progress.** A run-time loop in `always_ff` that stops making
  progress reports a runtime error naming its loop variable.
- **Memory files.** <a id="memory-files"></a> `$readmemh` and `$readmemb`
  read their file when they execute (21.4), never when the design is
  compiled, so compiled code, a JIT cache, and native images see the file
  as it is then. A load in an `initial` block that neither waits nor reads
  design state before it runs when each `Simulator` or `Simulation`
  instance is created, in both APIs; any other load runs at its statement,
  in `always_ff` at every activation and in `always_comb` whenever the
  block runs. A relative name is looked up in the working directory of the
  simulating process. The word-count warning and the errors of a load are
  `RuntimeEvent::Report`s; an error terminates the load, keeping the words
  loaded before it, and the simulation continues. An `initial` block that
  reads loaded words afterwards runs as a process. For the inferred-latch
  check of `always_comb`, a load assigns all the words it may load.
  An associative array is loaded when its indices are integral values of
  at most 64 bits and its elements packed values of at most 64 bits (21.4.1),
  always at its statement: each word creates or replaces the element of
  its address, which is the index's bit pattern (`@fffffffe` is the `int`
  index -2, as in Verilator). Without addresses in the call, a file address
  may name any index; an address or word that is no member of an
  enumerated index or element type is an error. An index that selects the
  array, as in `banks[i++]`, must have no effect. Dynamic arrays and queues
  are not loaded.

## Mixing Veryl and SystemVerilog {#mixed-language}

Mixed designs are a Celox feature, not part of the standard.
`Simulator::from_mixed_sources` (or
`Simulation::from_sources(veryl).with_sv_sources(sv)`) combines both source
lists. Veryl instantiates an SV module with `$sv::Module`; SV refers to a
Veryl module or interface by its emitted name (`prj_Module`, or `Module`
with `omit_project_prefix`; the project is `prj` without a `Veryl.toml`).

| Aspect | Supported across boundary | Not supported across boundary |
| --- | --- | --- |
| **Modules and ports** | Either language as top; bidirectional hierarchy crossing; module parameters and plain signal ports | — |
| **Interfaces and functions** | Interfaces and `input` / `output` modports crossing in both directions | Cross-language interface function calls; function `import` / `export` modports; interface header functions with statement bodies the contract cannot express (such as loops), header-dependent formal or return widths, declaration casts whose target size depends on interface parameters, and constant functions that cannot be lowered symbolically *(these remain available within a single language)* |

## DPI-C imports {#dpi-c-imports}

A design can call C functions declared with `import "DPI-C"`. `celox run`
links the shared libraries named by `--dpi-lib PATH` (repeatable) and by
`dpi_libs` in `celox.toml`. In the Rust API, DPI-C registration is a host
API of the builder:

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

Each import is linked by its C name at build time: `dpi_function`
registrations are searched first, then `dpi_library` libraries in order; an
unresolved name fails the build with a `Dpi` error. Both methods are
`unsafe`, because Celox cannot check that a C function matches its import.
`allow_unlinked_dpi_for_elaboration()` builds a design despite unresolved
imports, for an elaboration that never runs it (the sv-tests runner's
elaboration mode, which links no C code as other tools' does not): a call of
such an import is a run-time error that names the import and says it was
elaborated without its C body. It is off unless requested; simulation and
every other build keep the error.

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Declaration and call site** | `import "DPI-C"` functions, including `context` imports (35.5.3), declared in a module, package, or `$unit` scope, called from `initial`, `always` (including `always @*`), `always_ff`, and subroutines they call | Tasks; calls from continuous assignments or `always_comb`; using the C scope API (`svGetScope`, `svSetScope`, `svGetNameFromScope`, `svGetScopeFromName`, user data) or calling exported SV subroutines inside `context` imports (libraries using those symbols fail to load with an unresolved-symbol error) |
| **Arguments and return types** | Up to 16 arguments (`input`, `output`, or `inout`); `bit`, `logic` (`svBit` / `svLogic`), `byte`, `shortint`, `int`, `longint` (signed or unsigned), `chandle`, `string` (`const char *`), packed arrays of `bit`/`logic`/`reg`, `integer`, and `time` (`svBitVecVal` / `svLogicVecVal` words); results of the types before packed arrays, or `void` (35.5.5) | More than 16 arguments; `real`, `shortreal`, named types (`typedef`, enums, structs), and unpacked or open arrays |
| **`export "DPI-C"` (35.7)** | Declarations accepted and checked | Calling exported SV functions from C (see `context` import scope API restriction above) |

An `output` or `inout` argument is a pointer to its C type (a pointer to
`const char *` for `string`). A string or packed array passed to the
function is valid until it returns; a string it returns or writes to an
`output` is copied when it returns. In two-state simulation, unknown bits a
function writes to a `logic` argument read as zero. An `always @*` block
that calls an import runs when what it reads changes (9.4.2.2) and no more
often: a C function whose result changes without a change of its
SystemVerilog arguments is not called again until one changes.

UVM's C library functions are provided by Celox and shadow any user
implementation of the same name. The `uvm_re_*` functions use the Rust
`regex` crate, whose syntax differs from POSIX extended regular expressions
in rare constructs; `uvm_dpi_get_next_arg_c` returns `celox` followed by
the plusargs and `uvm_dpi_get_tool_name_c` returns `Celox`. The `uvm_hdl_*`
and `uvm_polling_*` functions return 0 or `null`, so UVM reports those
facilities as unavailable. UVM 1.2's `uvm_re_match`, `uvm_glob_to_re`,
`uvm_dump_re_cache`, `uvm_dpi_regcomp`, `uvm_dpi_regexec`, and
`uvm_dpi_regfree` are provided too. Their reports (`UVM/DPI/REGEX_INV`,
`UVM/DPI/REGEX_MAX`, `UVM/DPI/REGCOMP`, and the cache message) go through
the function the UVM package exports for them, `m__uvm_report_dpi`, as UVM's
C library makes them, so they count as UVM reports; the text after
`invalid glob or regular expression` is the `regex` crate's reason, not
`regerror`'s.

## SystemVerilog emitted by Veryl {#systemverilog-emitted-by-veryl}

The Veryl conformance suite also runs against the SystemVerilog that Veryl
emits. The cases that remain excluded are:

- Veryl emits invalid SystemVerilog for an array literal that mixes
  positional items with `default:` (`'{a, default: b}`).
- Designs that opt into Veryl function calls with output arguments or side
  effects in `always_ff` have no SystemVerilog equivalent and are not
  emitted.
- Testbench modules (`initial` blocks with clocking and `$finish`
  scheduling, hierarchical assignments) are not emitted as simulatable
  SystemVerilog modules.
