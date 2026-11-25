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
- **Port forms:** `ref` ports ([#427](https://github.com/celox-sim/celox/issues/427)).
- **Unions and mixed array/container types:** `tagged` unions, unpacked unions, packed union assignment patterns, fixed-size unpacked arrays of `chandle` (`chandle a[4]`, including via `typedef`), fixed-size unpacked arrays of dynamic containers (`int a[2][$]`, including via `typedef`), and dynamic containers of fixed-size unpacked arrays (`int a[][2]`).
- **Control flow:** `disable` statements other than `disable fork`; `fork` inside declaration initializers, `final`, `always_comb`, and `always_ff` (no process scheduler in those contexts).
- **Timing and system tasks:** `wait_order`, and any unimplemented IEEE 1800-2023 or Annex D system task/function. Celox knows every standard name and reports unsupported ones by name wherever written, including in unused parameters and function bodies (e.g., `$q_initialize`).

## Partially supported features {#partial-support}

For features with partial support, each table states both the **Supported** subset and what is **Not supported**.

### Ports, nets, `defparam`, and gate primitives {#ports-and-gates}

| Feature | Supported | Not supported |
| --- | --- | --- |
| **Wildcard `.*` port connections** ([#442](https://github.com/celox-sim/celox/issues/442)) | Wildcard `.*` connections to modules defined in the SystemVerilog sources (including modules with interface ports) | Wildcard `.*` connections to a module defined outside the SystemVerilog sources (such as Veryl) |
| **Nets with multiple drivers (6.6.1–6.6.3)** | Continuous assignments within one module to `wire`, `tri`, `wand`, `triand`, `wor`, or `trior` nets (resolved per 6.6.1–6.6.3. In 2-state simulation, without X/Z literals). Continuous assignments and net declaration assignments to `wire` or `tri` resolve drive strengths (28.12) | Multiple drivers across module instances (such as a tri-state bus across modules); drive strengths on `wand`, `triand`, `wor`, and `trior` with multiple drivers (28.12.4); `tri0`, `tri1`, and supply nets with several drivers; drivers calling a function with a data-dependent loop; in 2-state simulation, drivers with X or Z literals |
| **Undriven nets and `inout` ports** | Sub-module `inout` ports with one continuous driver on either side (e.g., `assign io = oe ? d : 'z;`); undriven bits of a net with an independent delay read `z` | Top-module `inout` ports; internal nets without a driver ([#460](https://github.com/celox-sim/celox/issues/460)) except undriven bits of a net with an independent delay |
| **Gate and switch primitives (Clause 28)** | `and`, `nand`, `or`, `nor`, `xor`, `xnor`, `buf`, and `not` without delays or drive strengths, single or in 1D instance arrays | All other gate and switch primitives (tri-state buffers, switches, pull sources); gate delays, drive strengths, and multidimensional gate instance arrays ([#457](https://github.com/celox-sim/celox/issues/457)) |
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
| **Dynamic container members (7.2.1)** | Dynamic array, queue, and associative array members (copied with the structure) | Declared initial values on container members; mixing fixed-size and dynamic dimensions (`int a[2][$]`, `int a[][2]`); `%p` of a struct with a container member; fixed-size arrays of such structs; mutating a container member of a struct stored inside another container (`q[i].m.push_back(x)`, `q[i].m[j] = x`, `q[i].m = x` — read-only there) |
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
| **Class `randomize()` and `randc` (18.3–18.4, 18.8, 18.11)** | 2-state and 4-state integral `rand` properties (any width unconstrained; 4-state `rand` takes 2-state values), scalar enums (declared values), fixed-size and dynamic random arrays of ≤ 64-bit 2-state integrals, inherited properties, `pre_randomize`/`post_randomize`, `rand_mode()`, `srandom()`, `get_randstate()`/`set_randstate()`, explicit member lists, `randomize(null)`; unconstrained `randc` on 1–16-bit 2-state integers and scalar enums up to 65,536 members | Aggregate properties (static or non-static); `randc` static properties or `randc` arrays; random arrays of elements other than ≤ 64-bit 2-state integrals; random object or `string` properties; `randc` domains > 16 bits or > 65,536 enum members; enums with unresolved/X/Z values |
| **Constraint blocks and expressions (18.5)** | 1–64-bit scalar integer properties (constant bit/part-selects of any declared range), comparisons, logical/bitwise ops, `+`, `-`, `*`; `if`/`else` and `->` (state guards evaluated before solving); `inside` (scalar values and inclusive ranges); `foreach`, `sum()`, and constant-index elements on random arrays (array sizes solved before elements/function calls; past-the-end reads 0); `dist` (constant weights/range bounds, `$` type bounds); `unique` on singular class properties of equivalent types; `soft` constraints and `disable soft`; `solve ... before`; `constraint_mode()`, inheritance, overrides; `==`/`!=` on class handles; single-hop handle members (`h.m`, `this.h.m`, `super.h.m`, `item.h.m`, static handles, selects `h.m[3:0]`/`hs[i].m[0]`); class functions with input arguments (18.5.11) | Constrained enums or `randc`; 4-state static properties read as state, 4-state members of other objects, or 4-state array elements in constraints; constraints naming static properties in a `rand`-handle-reached object; `sum() with`; `soft` inside `foreach`; `dist` with `default :/`, non-constant weights, or `:/` ranges with non-constant bounds; `unique` on other variables, arrays, or slices; deeper handle chains (`h.g.m`); function calls in constraint loops, inline constraints, `rand`-handle-reached objects, or arguments of another constraint call; typed parameters in constraints; array sets, X/Z wildcard items, or `$` bounds in `inside` sets |
| **Inline `randomize() with { ... }` (18.7)** | Same constraint subset; unqualified receiver properties, receiver handle variable (`h.randomize() with { h.x > 0; }`, read once at call start), caller-scope scalar inputs and expressions computed from caller variables and non-random receiver properties (`buffer[i]`, `req.rw`, `memory[rsp.addr]`, `m.get_size()`), and `local::` for non-property caller locals/arguments | Aggregate caller-scope inputs; typed parameter inputs; `local::` naming a property of the caller's class; caller expressions computed from a random variable (`memory[h.addr]` with `rand addr`); unqualified receiver `static` properties (rejected rather than falling back to a caller variable of the same name) |
| **`randsequence` (18.17)** | Non-recursive productions with weights, `rand join` (with bias; see [Random numbers](#random-numbers)), and `input` formals/return values of packed/scalar types | Recursive productions; code blocks after a weight; non-`input` formals; formals or values of unpacked, dynamically sized, or `event` types |

See [Random numbers](#random-numbers) for the solver policy, `soft`, `dist`, `solve before`, 4-state constraint semantics, constraint function calls, and the `rand join` bias formula.

### Dynamic containers {#dynamic-containers}

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Container and nesting shapes** | Dynamic arrays (`T a[]`), queues (`T q[$]`, bounded `T q[$:N]`), associative arrays (`T m[K]`), and nested dynamic containers (`int m[string][$]`, `T m[C][C][E]`) as module variables, locals, and class properties | Fixed-size unpacked arrays of dynamic containers (`int a[2][$]`, including via `typedef`); dynamic containers of fixed-size unpacked arrays (`int a[][2]`); container ports; `const` containers |
| **Element types** | Packed integrals of any width, `string`, class handles, `chandle`, supported unpacked structs (no fixed-size array or defaulted members), and nested dynamic containers | `real` / `shortreal` elements; named `event` elements |
| **Associative array keys** | Integral types of any width (including packed structs and enums), `string`, class handles, and `process` handles | Wildcard `[*]`; `real`, unpacked types, or other non-integral/non-handle key types |
| **Literals, `new[]`, and assignments** | Positional patterns, `{}` / element concatenations, `new[n]` with another compatible dynamic array/queue or an assignment pattern (including `'{default: v}` for scalar/non-container/non-struct elements), default-only associative literals (`'{default: v}`, 7.8.6), and blocking element or bit/part-select writes (integral ≤ 64 bits, side-effect-free container/index) | `new[n](fixed_array)`; `new[]` assignment pattern mixing `default:` with other keys or for container/unpacked-struct elements; explicit index entries or mixed index/`default:` associative literals; slices; keyed patterns; associative arrays inside concatenations; nonblocking element assignments; compound element assignments whose container or index has side effects or whose element is a `string`, handle, or container; writing a member of an unpacked-struct element directly (assign the whole element) |
| **Ordering and comparison (7.12.2)** | `reverse` and `shuffle` (dynamic arrays, queues, and 1D fixed-size arrays of ≤ 64-bit non-struct elements); `sort` and `rsort` (integral ≤ 64 bits and `string` without `with`; unpacked structs, class handles, and elements > 64 bits with a `with` expression on dynamic arrays/queues); `==`/`!=`/`===`/`!==` on containers with elements ≤ 64 bits (excluding structs with `string`/handle members) | `sort`/`rsort` without `with` on elements > 64 bits, unpacked structs, or class handles; ordering fixed-size arrays that are multidimensional, of unpacked structures, of elements > 64 bits, or with a `with` expression; `==` comparison on elements > 64 bits or structs with `string`/handle members |
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
| **Multiclocked sequences (16.13)** | Whole-property sequences joining single-path singly clocked subsequences via `##1` or `##0` across clock changes (value event controls incl. 4-state edges, `iff`, static virtual interface members; parenthesized clocking groups up to 64 nested groups, 16.13.3); `$sampled` inside subsequences | Multiclocked sequences inside implications, with `disable iff`, or with local variables/match items; named events as clocks; operators other than `##1`/`##0` across clock changes; `$past`/`$rose`/`$fell`/`$stable`/`$changed` in any subsequence; nesting beyond 64 groups |
| **Named properties and sequences** | Parameterless declarations in the same module outside generate blocks, whose body stays within this subset; a named sequence without local variables may be an operand of another sequence (same clock or leading) | Formal arguments; declarations in other scopes or generate blocks; instances of named sequences clocked differently or with local variables inside a multi-cycle sequence |
| **Function calls in assertions (16.6)** | Pure (side-effect-free) function calls in operands, `disable iff` conditions, and match-item values (arguments receive sampled values; body reads current values, 16.5.1) | Impure function calls (functions with `output`/`inout`/`ref`/`const ref` formals, non-local writes, retained static state, side-effecting system calls, or string/class method calls directly or transitively); DPI import calls; function calls in operands of `$past`/`$rose`/`$fell`/`$stable`/`$changed` |
| **Action blocks** | Untimed blocking statements, report/severity tasks, and subroutine calls that do not suspend, perform nonblocking assignments, start or control processes, or trigger events | Timed or nonblocking action statements; suspending, event-triggering, or process-controlling calls |

For `expect (property_spec) action_block` (16.17), the executing process makes one attempt starting from the next clocking event, decides each step on sampled values, runs the pass or fail statement (by default `$error`) in the Reactive region, and continues. Multi-path sequences pass at the first tick any path matches and fail at the first tick no path can still match; a missed antecedent succeeds vacuously at that tick.

### Coverage groups {#covergroups}

A covergroup (clause 19) is a type: declared in a module, a program, a package, the compilation unit or a generate block, or embedded in a class (19.4). `new` constructs an instance, with the arguments of the covergroup; `sample()` samples it, with the arguments of a `with function sample` declaration; `get_inst_coverage()`, `get_coverage()`, `T::get_coverage()` and `$get_coverage()` give coverage. The results are read through `Simulation::coverage()` (see [Coverage](#coverage)).

A construct of the **Not supported** column is rejected where a `new` or a method of the covergroup is written, not where the covergroup is declared: a covergroup that nothing constructs is accepted whatever it declares, and so is a variable of its type. All of them point to [#4294](https://github.com/celox-sim/celox/issues/4294), which tracks what is left of clause 19.

| Aspect | Supported | Not supported |
| --- | --- | --- |
| **Declarations (19.3, 19.4)** | A covergroup of a module, a program, a package, the compilation unit or a generate block; a covergroup embedded in a class; variables of a covergroup type | `covergroup extends` (19.4.1); a covergroup declared in an interface, which rejects the interface; one declared in a checker or in a loop generate block; constructing or sampling a covergroup type that a module takes as a type parameter (it declares variables there) |
| **Coverpoints, crosses, and options (19.5–19.7)** | Covergroups without items | A covergroup that declares a `coverpoint`, a `cross`, or an `option` or `type_option` assignment cannot be constructed, so the covergroups that can be have no item yet |
| **Sampling (19.3, 19.8.1)** | `sample()` | A covergroup declared with a clocking event (`covergroup cg @(posedge clk);`) or a block event (`@@(begin f)`); `sample()` in a sequence match item |
| **Arguments (19.3, 19.8.1)** | The arguments of a covergroup and of its `with function sample` declaration | `ref` arguments of a covergroup or of its sample method; arguments of unpacked type; an argument whose type or default names a `let` declaration |
| **Methods (19.8)** | `sample()`, `get_inst_coverage()`, `get_coverage()`, `T::get_coverage()`, and `$get_coverage()`; a method of an embedded covergroup called on its variable (`cg` in the class, `obj.cg`) | `start()`, `stop()` and `set_inst_name()`; `get_coverage` and `get_inst_coverage` with `ref int` arguments; the methods of coverpoints and crosses; reading or writing `option` and `type_option` members; a method of an embedded covergroup called on another expression that gives the handle, such as a conditional expression |
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

**Accepted even in strict mode (intentional extensions):**

- **Packed values and unbased fills assigned to fixed-size unpacked arrays
  (7.6).** Celox accepts assigning a packed value of the exact whole-array
  bit width (bit for bit) and filling every element with an unbased unsized
  literal such as `'0`. Values of any other width, `real`, or `string` are
  rejected.
- **Procedural `force` on variable selects and unpacked array elements
  (10.6.2).** Extends `force` beyond vector nets to constant bit/part-selects
  or members of variables, and constant-indexed elements (or constant
  selects/members) of fixed-size unpacked arrays of packed integrals
  (emitting `forced_array`).
- **`$psprintf` is accepted as `$sformatf`.** The vendor system function
  `$psprintf` is accepted as `$sformatf` (21.3.3); diagnostics name it
  `$sformatf`.

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
  variable. `get_coverage()` through a handle gives the coverage of the type
  and does not read the handle; `get_inst_coverage()` on `null` is a runtime
  error.
- **A covergroup without items has no coverage.** `get_inst_coverage()` and
  `get_coverage()` of a covergroup that declares no coverpoint or cross are
  0, and it takes no part in `$get_coverage()`, which is 100 when no
  covergroup of the design does (19.11).
- **Coverage is cumulative and instances are never freed.** The samples of
  an instance stay in the coverage of its type and in the report when
  nothing refers to it any more; the report marks it as no longer live. A
  testbench that constructs a covergroup for each transaction grows the
  report by an instance each time.
- **Constructing a covergroup draws no random number**, so adding one to a
  procedure or to a class constructor leaves its random stimulus unchanged
  (18.14.1). A covergroup variable declared with an initializer in a
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
  in its state. Covergroups are not available in the browser (WASI) build,
  which has no heap.
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
