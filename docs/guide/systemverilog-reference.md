---
outline: [2, 3]
---

# SystemVerilog Deviations

For setup and a first simulation, start with [SystemVerilog Support](./systemverilog).

## Baseline: IEEE 1800-2023 {#what-is-supported}

Celox implements SystemVerilog as specified by IEEE Std 1800™-2023.
This page does not restate the standard.
It lists only where Celox differs:

1. **Unsupported** constructs, which are rejected at compile time.
2. **Intentional deviations**, where accepted code behaves differently.
3. **Implementation-defined choices**, where the standard leaves the
   behavior to the implementation or unspecified.
4. The **Celox execution model**, which has no counterpart in the standard
   (the Rust `Simulator` / `Simulation` split, checkpoints, mixed-language
   designs, DPI-C registration).

Anything not listed here follows the standard.
If you observe a difference that is not listed, please
[file an issue](https://github.com/celox-sim/celox/issues): treat it as a bug,
not as undocumented intended behavior.
Measured conformance is published on the [verification page](/verification/).

An unsupported construct reports an `Unsupported` error naming the construct
instead of being silently ignored.
The error carries the number of the issue that tracks the construct;
constructs without a dedicated issue point to the frontend roadmap,
[#88](https://github.com/celox-sim/celox/issues/88).

## Unsupported constructs {#what-is-not-supported}

### Design structure

- `bind`, `checker`, `config`, `covergroup`, clocking blocks and clocking
  drives, user-defined primitives (UDP), and `specify` blocks. A class may
  declare a covergroup that nothing constructs; constructing one or calling
  its methods is rejected.
- `defparam` assignments other than to a parameter of a module instance of
  the module that has the statement, which may be in a generate block:
  names through a child instance, names that start at `$root`, elements of
  instance arrays, interface instances, and parameters of the statement's
  own scope.
- Ports and instances: non-ANSI port expressions such as
  `module m({a, b}, c[3:0], .p(d))`
  ([#1953](https://github.com/celox-sim/celox/issues/1953)) (plain non-ANSI
  ports — a header of port names with body declarations — are supported),
  `ref` ports ([#427](https://github.com/celox-sim/celox/issues/427)),
  wildcard `.*` connections to a module defined outside the SystemVerilog
  sources ([#442](https://github.com/celox-sim/celox/issues/442)), and ANSI
  port default values.
- Nets with several drivers, other than the continuous assignments of one
  module to a `wire`, `tri`, `wand`, `triand`, `wor` or `trior` net, which
  are resolved (6.6.1 to 6.6.3). Still rejected are drivers below an
  instance (so a tri-state bus across modules), assignments with a drive
  strength, `tri0`, `tri1` and supply nets with several drivers, drivers
  that call a function with a loop whose condition depends on values, and,
  in two-state simulation, drivers with an X or Z literal, whose high
  impedance a two-state value cannot hold. `inout` ports of the top module
  are rejected. An `inout` below the top is supported only with one
  continuous driver on either side, such as `assign io = oe ? d : 'z;`.
  Internal nets without a driver are rejected
  ([#460](https://github.com/celox-sim/celox/issues/460)), except undriven
  bits of a net with an independent delay, which read as `z`.
- Gate and switch primitives other than `and`, `nand`, `or`, `nor`, `xor`,
  `xnor`, `buf`, `not` without delays or strengths, single or in
  one-dimensional instance arrays (Clause 28); gate delays and strengths are
  also unsupported ([#457](https://github.com/celox-sim/celox/issues/457)).
- Instance arrays with a negative bound.
- Hierarchical references <a id="hierarchical-references"></a> that drive a net or a
  variable through a continuous assignment, appear in port connections, or
  name tasks, functions, named blocks, parameters, structure members of
  another scope other than members of a packed structure variable (a member
  of an element of a packed array of structures is rejected), or `real`,
  `string` (except through the host API name, see below), or class-typed
  signals; names that start inside a generate block relative to it are also
  rejected. A static variable of a subroutine is named as `f.v` (23.6) only
  from a scope that `f` is visible in by its simple name: a method of the
  class or a base, or a subroutine of the design element; not through an
  instance, package or named block, and not when the variable is of
  unpacked structure type. A name that the body of an interface instance
  does not declare is searched for from the scope that instantiates the
  instance (23.8); it is rejected where the body is reached through an
  interface port, in the tasks and functions of an interface that a virtual
  interface names, and in an interface instance that another interface's
  body declares.
- Packages: package nets
  ([#1146](https://github.com/celox-sim/celox/issues/1146)); a package
  variable cannot be a clock or reset.
- Compilation units: only one source file may declare `$unit` variables.
- `let` declarations (11.12) only among the items of a module, interface or
  program, where each call is expanded; one in a package, a generate block
  or a procedural block is rejected, and so is a call in a scope that
  declares a name of the body again.

### Interfaces {#interface-restrictions}

- **Declarations:** implicit nets, enums, module instances, macros,
  and compiler directives inside an interface. Recursive interface
  specializations and nesting beyond 64 levels are rejected.
- **Modports and connections:** `inout`, `ref`, expression ports, `export`,
  and clocking modports. A connection cannot name a modport if the receiving
  port declares none. Writes through a port without a modport, and member
  connections from such a port to a non-ANSI module, are unsupported.
- **Static storage of subroutines:** a static function or task called
  through interface ports of several modules keeps its static variables, and
  the static formals of a task that suspends, once per calling module
  instead of once per interface instance (13.3.2, 13.4.2;
  [#3739](https://github.com/celox-sim/celox/issues/3739)). Calls through
  the instance itself and through the port of a single module share one.
- **Declaration dependencies:** member references in parameter, constant,
  type, or member declarations of an interface used through a port;
  compilation-unit items used by an interface when a module in another file,
  or before the interface, uses it.
- **Names and hierarchy:** references to implicit generate names (`genblk1`)
  and paths through generate blocks (`g.h.x`); declarations that reuse an
  interface instance or port name; package names brought in by interfaces
  that collide with another imported package's item or a module declaration.
- **Identifiers:** designs with interfaces cannot use `$` in their own
  names, or escaped identifiers that are not simple identifiers; expansion
  reserves `$` for generated names.
- Variable declaration initializers of members of an interface array.
- Virtual interfaces <a id="virtual-interfaces"></a>: a `wait` statement
  through a virtual interface held by an automatic variable or an argument,
  or by a class property of an object one holds (a property of `this` or of
  an object a static variable holds is watched); virtual interfaces of
  parameterized interfaces;
  clocking blocks and nested interface instances reached through a virtual
  interface; writes to nets through a virtual interface; `$bits` of a call
  of an interface function through a virtual interface in a package or
  class, whose analysis precedes the interface's subroutines (in a module
  it is the function's width;
  [#4056](https://github.com/celox-sim/celox/issues/4056)).
- An interface whose default lifetime (`interface automatic`) differs from
  that of the module it is used in.

### Programs {#programs}

`program` blocks (Clause 24) run their `initial` procedures in the reactive
region set. Not supported: programs nested in modules and anonymous
programs; an uninstantiated program as an implicit top level; clocking
blocks in programs. Two region deviations are listed under
[intentional deviations](#intentional-deviations).

### Types and expressions

- Tagged unions, unpacked unions, and packed union assignment patterns.
- Fixed-size unpacked arrays of `chandle` (`chandle a[4]`, also through a
  typedef).
- Enum methods (`first`, `next`, …) on an element of a packed array of enums
  ([#2417](https://github.com/celox-sim/celox/issues/2417)).
- Reading a constant part-select that lies outside the vector, and writing
  a part-select of an array element or of an inner packed dimension that
  leaves its declared range
  ([#88](https://github.com/celox-sim/celox/issues/88)). Run-time selects
  follow the standard.
- Reduction operators in parameter expressions
  ([#456](https://github.com/celox-sim/celox/issues/456)).
- Type operator (6.23): a type reference compared with a value, range items
  in a `case` on a type, and a type operator applied to an expression that
  reads a port, or to any expression in a generate block.
- Type names <a id="type-names"></a>: `$typename` (20.6.1) rejects types without one
  exact spelling instead of inventing system-generated names: anonymous
  enumerations, structs and unions, soft and tagged unions, unpacked
  structures with unpacked array members, packed arrays of enumerations or
  of signed typedefs, classes with non-integral value parameters, and
  operands other than names and data types, such as `$typename(a + b)`.
  A virtual interface type, which 20.6.1 has no rule for, is written as
  it is declared: `virtual bus_if` or `virtual bus_if.mp`.
- Parameters of unpacked array type hold their value from the start of the
  simulation. An element of a one-dimensional array parameter is an
  elaboration-time constant when the elements are packed integral values
  of at most 127 bits without X or Z bits and the array has at most 4096
  of them, and so is a select of such an element's packed dimensions
  unless the element type is a packed array of a signed named type; the
  whole array, and an element of any other array parameter, cannot be used
  where such a constant is required. A member of a packed-structure
  parameter is such a constant
  unless it is wider than 127 bits or has X or Z bits: then it cannot size
  a declaration, select a generate construct or override a parameter.

### Unpacked structures {#unpacked-structures}

- Unpacked structs as ports, nets, or parameters, or as `static`, `rand`,
  or `const` class properties.
- Fixed-size array members of enum, packed aggregate, `string`, handle, or
  unpacked structure type, or with declared initial values. Structures with
  fixed-size array members cannot be ports, class properties, subroutine
  interfaces, or dynamic container elements.
- Dynamic array, queue, and associative array members (7.2.1) with
  declared initial values or with a fixed-size dimension before or after
  the dynamic one (`int a[2][$]`, `int a[][2]`, as for any container of
  fixed-size arrays); `%p` of a structure with such a member; fixed-size
  arrays of such structures. Such a member is a container of its own,
  copied with the structure; a structure with one, at any depth, cannot be
  a port. Of a structure element of a container, it can
  only be read: method updates, element writes and assignments
  (`q[i].m.push_back(x)`, `q[i].m[j] = x`, `q[i].m = x`) are unsupported.
- Fixed-size arrays of structures whose members are `string` or handle
  typed, mix 2-state and 4-state leaves, or declare initial values; such
  arrays as ports, class properties, or subroutine interfaces.
- Member initial values on enum or non-integral members, whole
  nested-structure member initializers, and defaulted structure types in
  module ports, subroutine interfaces, or dynamic containers.
- Members that are `real` or arrays outside the cases above.

### Processes, timing, and control flow {#timing-controls}

- The `disable` statement, except `disable fork`.
- Procedural `force` / `release` (IEEE 1800-2023 10.6.2) outside `initial`
  and `always` procedures: in a subroutine, a `final` procedure, or an
  `always_comb`, `always_latch` or `always_ff` procedure. An `always`
  procedure with one runs as a process. The target is a packed integral
  variable or net of the module or, through a hierarchical name, of another
  instance, or a constant bit-select, part-select or member of one: as an
  extension, a constant select or a member of a variable is a target too,
  which the standard allows only of a vector net. A force of an input port
  of an instance forces that port, not the net connected to it. Every
  instance of a module whose variable or net a hierarchical name forces
  carries the force controls, which the warning `forced_specialization`
  reports when several instances share them. A write of a forced target
  through a hierarchical name from a module that does not force it is not
  overridden. A concatenation, a select
  with nonconstant bounds, a `real`, `string`, handle or unpacked target (singular ones
  the standard allows), and a variable that a nonblocking assignment with
  an intra-assignment delay writes are rejected, as are a target that the
  `always_comb`, `always_latch` or `always @*` procedure assigning it also
  reads, a target that an `always_comb` running as a process (calling a
  task, for example) assigns whole, and overlapping parts
  of one target when a force of one of them has an expression that reads a
  variable. So are an expression with side effects or with a call of a
  subroutine or a system function, and one that reads a variable of a
  procedural block. A target that the host also forces
  (`Simulator::force`) is rejected, as is an input port of the top module,
  which the host writes.
- Procedural continuous assignments (`assign` / `deassign`, IEEE 1800-2023
  10.6.1) outside `initial` and `always` procedures: in a subroutine, a
  `final` procedure, or an `always_comb`, `always_latch` or `always_ff`
  procedure. An `always` procedure with one runs as a process. The target is
  a whole packed integral variable of the module: a concatenation, a `real`,
  `string`, handle or unpacked variable, and a variable of a procedural
  block are rejected, as are an expression with side effects or with a call
  of a subroutine or a system function, an expression that reads a variable
  of a procedural block, and a target that an `always @*` procedure also
  assigns.
- While an `assign` is active, a write to its variable other than by a
  plain assignment statement (an output argument, a concatenation target, a
  delayed nonblocking assignment, a hierarchical name in another module) is
  overridden in the time step it happens, not suppressed: a process that
  waits on the variable can see the written value.
- Nonblocking assignments inside `always_comb`
  ([#453](https://github.com/celox-sim/celox/issues/453)) and inside
  `always_latch` ([#431](https://github.com/celox-sim/celox/issues/431));
  use blocking assignments there.
- `iff` qualifiers in `always_ff` event lists
  ([#452](https://github.com/celox-sim/celox/issues/452)), `always_ff` edge
  operands other than a plain signal
  ([#464](https://github.com/celox-sim/celox/issues/464)), multi-bit edge
  signals in `always_ff`, and several `always_ff` edges without an
  asynchronous reset. The same shapes in a plain `always` run as timed
  processes instead; see the [execution model](#celox-execution-model).
- Intra-assignment timing: nonblocking intra-assignment event controls and
  nonconstant delays. Delayed nonblocking assignments
  (`lhs <= #constant rhs`, 10.4.2) accept only integral constant delays and
  time literals, in `initial` and ordinary `always` processes.
- Continuous assignment delays <a id="continuous-assignment-delays"></a> beyond one
  nonnegative integral constant or time literal
  ([#444](https://github.com/celox-sim/celox/issues/444)): rise/fall/turn-off
  delay lists, variable or negative amounts, unitless fractional real
  amounts, concatenated or aggregate destinations, dynamic destination
  selects, destinations that are not scalar or packed integral, and a
  right-hand side with subroutine calls. Independent net delays
  (`wire #5 w;`) do not support delayed nets in instance port connections,
  delayed input/`inout` ports, unpacked or non-integral delayed nets, or
  drivers with dynamic destination selects.
- Timing controls in processes: impure function calls in event expressions;
  `wait` or event expressions reading a static task argument; event
  expressions reading a class property inside a larger expression, through a
  function, or in a container held in one (an item that is a property, such
  as `@(posedge h.p)`, is supported); an `iff` on such a property item that
  reads anything other than constants and what the waiting process alone
  writes, and an expression item, or an `iff` other than a one-bit variable,
  in the same event list as a property item; `wait` conditions reading
  `rand_mode()` or `constraint_mode()`; `wait` conditions over class
  properties that call a task or an effectful function, or read the time, a
  random number, a mailbox, or a process.
- Package and `$unit` timed tasks with static arguments.
- `fork` in declaration initializers, `final`, `always_comb`, and
  `always_ff` (these contexts provide no process kernel for scheduling
  calls). Compound event expressions over fork-shared automatic locals
  reject mixed global/static dependencies, foreign calls, functions that
  retain scratch state, and implicit writes to a watched operand from a
  function, a seed/plusargs result, or a system task such as `$readmemh`.
- Named events <a id="named-events"></a> beyond the basics: arrays of events, event
  ports, event members of unpacked structures, functions returning an
  event, triggers in `always_comb` / `always_latch`, `->>` with a delay or
  event control, event controls mixing an event with signal changes or
  edges (`@(e or posedge clk)`), `triggered` in a `wait` condition with
  other operands or read by a function the condition calls, and
  `wait_order`.
- Typeless mailboxes (`mailbox` without a type parameter); mailbox element
  types beyond integral types up to 64 bits, `string`, and class,
  `mailbox`, `semaphore`, `process`, or `event` handles.
- Constructing or extending the built-in `process` class; suspending its
  own thread or calling `await()` from a function.
- Loops: a procedural loop unrolls at most 10,000 iterations; a
  combinational loop with a run-time trip count must be a counted `for`
  loop; loop-generate constructs past 10,000 iterations are rejected
  ([#448](https://github.com/celox-sim/celox/issues/448)); genvar updates
  with bitwise compound operators are rejected
  ([#455](https://github.com/celox-sim/celox/issues/455)); recursive
  constant-function expansion in generate constructs is rejected.
- Final procedures <a id="final-procedures"></a>: timing controls and calls to user
  tasks in a `final` procedure, including through functions; functions that
  schedule events after returning (13.4.4). Detached `fork … join_none`
  children declared in a `final` procedure are initialized but never start.
  See also the [execution model](#celox-execution-model).

### Classes {#classes}

- `$cast` to a variable that is not a class handle.
- Anonymous enumeration, struct, and union types, and named four-state or
  string constants of a module or package, as class parameter values (a
  class passes its own such parameters on by name).
- Module-local classes as type arguments to package-class specializations,
  and anonymous enumeration, struct, or union types as type arguments.
- In an override, by a class of a module, of a virtual method of a class
  declared outside the module (which other scopes call through handles of
  that class): reading a module variable or port of a type other than an
  integral type without unpacked dimensions or a class handle, static
  variables, and calls of the module's own subroutines that read its
  variables.
- Properties that are `real`; fixed-size array properties that are
  `static` or `randc`, have more than one unpacked dimension or a
  negative bound or one outside `int`, are assigned or compared with an
  array other than another such property or an array variable named by
  itself (or, for elements of unpacked structures, such a variable at all),
  are assigned a keyed assignment pattern or an unpacked array
  concatenation with an array among its items, are sliced, or are passed to a `ref`, `output` or `inout` formal; a constraint `foreach`
  over a fixed-size array property whose range is not `[0:N-1]`; array
  locator methods on a fixed-size array property of unpacked structures
  whose range is not `[0:N-1]`; packed structure members of a property;
  selects of a member of a property of unpacked structure type; reads of
  bit and part selects of a property not declared `[N-1:0]` outside
  constraints, which take selects of any declared range (see Randomization
  and constraints); nonblocking
  assignments to a property; properties as `output` or `inout` arguments;
  `const` dynamic container properties.
- An unqualified static-property name in an inline `randomize() with`
  constraint does not fall back to a caller variable of the same name; it
  is rejected.

### Randomization and constraints

- Scope `randomize()` / `std::randomize()` (18.12): `with` constraints on
  other than integral variables of at most 64 bits, such as enums, enums
  with unsupported or unresolved domains, unpacked arrays, nets,
  constants, selects, and concatenation arguments.
- `randsequence` (18.17): recursive productions, code blocks after a
  weight, and production formals and values of unpacked, dynamically sized
  or event types or formals that are not inputs.
  A production of several rules picks one by their weights, 1 unless given,
  from the thread's random number generator (18.14.2). `rand join`
  interleaves its productions' items at random from the same generator.
  18.17.5 gives the bias no formula. Celox evaluates it once and, at each
  step, with probability |2 × bias − 1| advances a production with the
  fewest items left (bias below 0.5) or the most (above 0.5), and
  otherwise any production with items left, each as likely. So 0.5 (the
  default) prefers no length, and 0.0 and 1.0 always prefer the shortest
  and the longest.
- Class `randomize()`: aggregate properties, static or not, 4-state static
  properties a constraint reads as state, 4-state members of other objects
  and 4-state array elements in constraints, `randc` static properties,
  constraints that name static properties in an object that a rand handle
  reaches (its randomization fails), random arrays of elements other than
  2-state integral ones of at most 64 bits, `randc` arrays, random object
  and string properties, constrained enums and `randc`, `sum() with`, `soft`
  constraints in a `foreach`, `dist` with `default :/`,
  weights other than constant expressions or ranges weighted per value with
  bounds other than constant expressions, typed parameters in constraints,
  array sets, X/Z wildcard items and `$` bounds in `inside` sets, and
  `randc` domains beyond 1–16-bit integers and scalar enums with up to
  65536 members. Named constraint blocks cover only 1–64-bit scalar integer
  properties with comparisons, logical and bitwise operations, addition,
  subtraction, and multiplication, bit-selects and part-selects of a
  property at constant positions, class handle properties compared with each
  other or `null` by `==` and `!=`, and calls of functions of the class with
  input arguments (18.5.11); a call in a constraint loop, in inline
  constraints, in the constraints of an object a rand handle reaches, or in
  an argument of another is unsupported. The elements of a random array,
  fixed-size or dynamic, that a `foreach`, a `sum()` or an element at a
  constant position of a constraint names are solver variables (18.5.8); the
  others are drawn at random. An array whose size a constraint names too has
  its size solved first, without the constraints on its elements, which then
  take it as state (18.5.7.1), so they can leave the call no solution; those
  sizes are solved before the arguments of the functions the constraints
  call (18.5.11); an element past the end reads as 0. A `dist` allows the
  values of its items of nonzero total weight, a range bound `$` being the
  lowest or highest value of the type of its expression; the solver first
  tries one item of each `dist`, picked by its weight (one under `if` or
  `->` where its condition holds, one in a `foreach` for each element), and
  then any allowed value, so the weights bias the values without sampling
  them exactly. A `unique` constraint takes singular properties of the class
  of equivalent types; other variables, arrays and slices in it are
  unsupported. A constraint may name a member of the object a handle
  property refers to (`h.m`, 18.5.9), the handle named as any property is,
  as `this.h.m`, `super.h.m` or the receiver's `item.h.m`, a static handle
  too: a variable solved with the object through a rand handle, a state
  value through another;
  longer chains (`h.g.m`) are unsupported. A select of such a member or of
  a member of a loop element, as `h.m[3:0]` or `hs[i].m[0]`, takes its bits
  in every constraint position, `solve ... before` and `disable soft`
  included, the latter naming the member. A guard of state values, as of `if` or `->`, is
  evaluated before solving, so a constraint it leaves out reads no member of
  a null handle (18.5.13).
- Inline `with` constraints: aggregate caller-scope inputs, typed parameter
  inputs, a `local::` name of a property of the caller's class, and a value
  the caller computes from a random variable (18.7). An integral value the
  caller computes from its variables and from properties of the object that
  are neither `rand` nor `randc` is an input the call reads as it starts,
  after `pre_randomize()`: an element at a position a caller-scope name or a
  literal gives (as `buffer[i]`), a member of a caller-scope variable (as
  `req.rw`), an element at a position such a value gives (as
  `memory[rsp.addr]`), and a method call on an object (as
  `m.get_size()`). `memory[h.addr]` with a `rand addr` stays unsupported.
  The variable that holds the object being randomized, as `h` of
  `h.randomize() with { h.x > 0; }`, names that object, also in
  `foreach (h.a[i])`, `h.a[i]` and guards. That variable is read once as
  the call starts: a `pre_randomize()` that assigns it does not change the
  object its names refer to.
- Enums with unresolved or X/Z values for randomization.

### Dynamic containers, strings, and reference arguments

- Containers: multidimensional dynamic containers in some operations,
  `real` elements, wildcard or class keys beyond the supported key types,
  slices, keyed patterns, associative arrays within concatenations,
  nonblocking element assignments, compound element assignments whose
  container or index has a side effect or whose element is a `string`,
  handle, or container, container ports, and `const` containers.
  A `new[]` initializer is another compatible dynamic array or queue, or an
  assignment pattern, whose `default:` alone gives every new element its
  value; fixed-size arrays, and `default:` with other keys or for elements
  that are containers or unpacked structures, are unsupported. Explicit index entries, and mixtures
  of index entries with `default:`, in associative array literals are
  unsupported (a default-only literal is supported, 7.8.6).
- Container methods: locator methods returning indices for integral keys
  wider than 64 bits; sorting elements wider than 64 bits without a `with`
  expression; ordering a fixed-size array variable of more than one
  dimension, of unpacked structures or of elements wider than 64 bits, or
  with a `with` expression; comparing containers of elements
  wider than 64 bits with `==`, reducing them, or printing them with `%p`;
  type queries (`$bits`, …) on string- or class-valued queue pop results;
  queries of element dimensions and associative bounds; locator methods in
  continuous assignments, on multidimensional fixed arrays, or as discarded
  statements; reduction and locator `with` expressions and iterator
  arguments outside the locator subset above.
- Container formals: function results of container type, class properties
  and container elements as `output` / `inout` actuals, and actuals whose
  element types differ from the formal's.
- Strings <a id="strings"></a>: `atoreal` / `realtoa`; a string variable as
  the format of `$value$plusargs`; string arguments of file I/O;
  string values in VPI, VCD output,
  and the TypeScript API; nonblocking character writes to fixed string
  arrays; method calls on whole string arrays or subarrays; array-valued
  string arguments or return values.
- Reference arguments <a id="reference-arguments"></a> (13.5.2): `ref static`;
  elements of dynamic arrays, queues, and associative arrays as actuals;
  elements of class properties; writes to part of a formal bound to a class
  property or to a member of one; selects of a formal bound to an array
  element at a position only known at run time.
- Named subroutine arguments in calls whose selection indices have
  procedural effects; a method default that uses the receiver in an
  `always_comb` or `assign` call when the receiver, or an earlier argument
  that reads the object, has effects.

### Assertions

- Deferred immediate assertions (`assert #0`, `assert final`, …): an action
  must be one display or severity task call; a user subroutine, or more
  than one statement, is rejected.
- Assertion control tasks (IEEE 1800-2023 20.11) other than `$asserton` and
  `$assertoff` without arguments, which stop and restart the checking of
  every assertion of the design: `$assertkill`, `$assertcontrol`, the
  action control tasks (`$assertpasson`, …), and a level or a list of
  scopes or assertions as arguments. `$asserton` and `$assertoff` are
  rejected in a package or at compilation-unit scope; assertions in the
  subroutines of a package and in classes declared there are not
  controlled. An `always_comb` or `always_latch` that calls them is
  rejected when any procedure other than an `initial` or edge-controlled
  one calls them too. In a design that calls them, a concurrent sequence
  with more than one way to match (a cycle-delay range or `or`) is
  rejected.
- Sampled values <a id="sampled-values"></a>: `$past`, `$rose`, `$fell`, `$stable`,
  and `$changed` with an explicit clocking event argument, without a clock
  to infer, nested (`$past($past(a))`), or with operands that read an
  unpacked array.

### Concurrent assertions {#concurrent-assertions}

Only the following subset is supported; everything else in Clause 16
concurrent assertions is unsupported:

- Module and generate items with Boolean `assert property`,
  `assume property`, and `cover property` with an explicit clock, and
  sequences of steps a fixed number of clock ticks apart (`a ##1 b`),
  spanning at most 1024 ticks, joined by `|->` or `|=>`.
- Cycle-delay ranges with an upper bound (`a ##[1:3] b`, also leading),
  `or`, `and` and `intersect` of sequences, and `b throughout s`, without
  match items in their operands: an attempt passes at the first tick one of
  its ways to
  match matches, and fails at the first tick none can match any more. At
  most 1024 ways to match, and only in a sequence that is the whole
  property: not in an implication, an `expect` statement, or one with local
  variables or match items.
- `disable iff` of a sequence over cycles: an attempt is disabled, and
  neither passes nor fails, when the condition is true at any time from
  its start to its end, also between the ticks of the clock, over the
  current values of its variables (16.12).
- Multiclocked sequences (16.13.1) that are the whole property: singly
  clocked subsequences, each with one way to match, joined by `##1` or `##0`
  at a change of clock (`@(posedge c0) a ##1 @(posedge c1) b`), each clock an
  event control over values, which ticks as it would wake a process (4-state
  edges, `iff` judged when the event occurs, a member selected through a
  virtual interface that a variable of the design holds), not a named
  event. After `##1` the next subsequence starts at the nearest tick
  of its clock strictly after the end of the one before, after `##0` also at
  it. A clocking event in parentheses clocks the rest of its group, and the
  clock around the group resumes after it (16.13.3), as in
  `@(posedge c0) a ##1 (@(posedge c1) b) ##1 c`. Not with `disable iff`, in an implication, or with local variables or
  match items; other operators across a clock change are rejected, and so
  are `$past`, `$rose`, `$fell`, `$stable` and `$changed` in any
  subsequence (`$sampled` is accepted). A sequence nests at most 64 groups
  and clocked subsequences.
- `p iff q` of two Boolean expressions, also as the consequent of `|->`
  (`a |-> b iff c` is `a |-> (b iff c)`).
- `expect (property_spec) action_block` (16.17) in a process or task, with an
  explicit clock: the process makes one attempt from the next clocking
  event, deciding each step on sampled values, then runs the pass or fail
  statement (by default `$error`) in the Reactive region and goes on. The
  property is a Boolean one, an implication or a sequence of fixed delays,
  also a named one, without `disable iff`, local variables or match items.
- A named property or sequence without formal arguments, in the same module
  outside generate blocks, whose body stays within this subset; local
  variables of built-in integral types and match items. A named sequence
  without local variables may be an operand of another sequence, clocked as
  it or leading it.
- Untimed blocking actions, report/severity tasks, and subroutine calls
  that do not suspend, perform a nonblocking assignment, start or control a
  process, or trigger an event.
- Calls of functions without side effects (16.6) in operands, `disable iff`
  conditions and match-item values. A function gets the sampled values of
  its arguments, and its body reads current values (16.5.1).

Unsupported in particular: procedural concurrent assertions,
implicit/default clocking, named properties and sequences with formal
arguments or in other scopes, unbounded cycle-delay ranges (`##[1:$]`),
repetition, the sequence operator `within`, `intersect` of sequences that
never match at one tick, instances of named sequences clocked differently
or with local variables inside a
multi-cycle sequence, `iff`
with an operand over cycles or a named operand, calls of functions that
are not pure (an output, inout, ref or const ref formal of the called
function; writes to non-local variables, kept static state,
side-effecting system calls, or string or class method calls, directly
or through the functions it calls), DPI import calls, function calls in
the operand of `$past`/`$rose`/`$fell`/`$stable`/`$changed`, and timed
or nonblocking action statements.

### System tasks, functions, and I/O

- Celox knows every system task and function name IEEE 1800-2023 defines,
  including Annex D. One it does not support where it is called, such as
  `$get_coverage`, is reported by name wherever it is written, including in
  an unused parameter and in a function body.
- `%t` of a `string` value; time queries
  (`$timeunit`, `$timeprecision`, `$printtimescale`) of selected instance
  array elements and generate paths.
- Runtime I/O calls (`$fopen`, `$display` to files, `$system`, plusargs, …)
  from combinational processes and continuous assignments; `$sformatf`
  family calls from combinational processes and continuous assignments;
  `$system` on WebAssembly hosts.
- File I/O subsets: `$strobe` / `$fstrobe` and `$monitor` / `$fmonitor`
  accept only module/static expressions in scheduled processes (calls in
  subroutines, procedural locals, unrolled loop variables, updates, and
  effectful calls in arguments are rejected); `$monitor` observes only
  whole static scalar variables, console `string` variables, constants,
  and direct time queries. `$fgets` into packed buffers, class properties,
  or dynamic containers; `$ferror` to class properties or dynamic
  containers; `$sscanf` / `$fscanf` `%s` outputs and `%t` scaling;
  `$sformat` / `$swrite` to an unpacked array of `byte`; dynamic `string`
  and non-UTF-8 file names and modes; `$writememh` / `$writememb` of
  dynamic and associative arrays.
- Formatted output <a id="formatted-output-to-strings"></a>: `%p` of an enum value
  returned by a function call or popped from a queue, packed unions, a packed
  structure returned by a function call or computed by another call with
  side effects, packed arrays of enums, fixed-size unpacked arrays, structures with
  `real` members, other aggregate function results, and containers of other
  aggregates. `%p` with a run-time format prints a string in quotes and any
  other value as `%0d`.
- Plusargs: `real` conversions and a string variable as the format of
  `$value$plusargs`. A `string` destination takes the `%s` conversion only.

## Intentional deviations {#intentional-deviations}

These are accepted by Celox but do not follow the standard exactly.
Code that must stay portable should avoid relying on them.

- **A missing `static` keyword is forgiven in one case.** IEEE 1800-2023
  6.21 requires an explicit `static` when a static variable's declaration
  specifies an initialization value. Celox also accepts the declaration
  without the keyword with a constant initializer in any procedure, and
  with a run-time initializer in a procedure that runs once, such as
  `initial`. In an `always` procedure a run-time initializer without the
  keyword is rejected; declare the local `static` or `automatic`. Both
  accepted forms report an `implicit_static_initializer` warning, which the
  diagnostics policy can silence (`allow`) or escalate (`error`).
- **Packed values and fills of fixed-size unpacked arrays.** IEEE
  1800-2023 7.6 assigns a fixed-size unpacked array only an unpacked array
  of a compatible type. Celox also assigns it a packed value exactly as
  wide as the whole array, bit for bit, and fills every element with an
  unbased unsized literal such as `'0`. A value of any other width, a real or a string
  variable is rejected.
- **Program continuous assignments do not run in the Reactive region.**
  Continuous assignments in a program are evaluated with the design's
  combinational logic, and design logic that a program writes settles while
  the reactive region set runs, so a program resuming after `#0` reads the
  new values. IEEE 1800-2023 4.5 evaluates them only once the reactive
  region set is empty. Icarus, Verilator, and xezim behave as Celox does.
- **`$exit` outside a program warns.** A `$exit` call from anywhere other
  than a program thread is ignored, as 24.7 specifies, but Celox also
  reports a compilation warning (`Simulator::warnings()`), including for
  calls in unused function declarations.
- **Coverage is not reported.** Immediate `cover` executes its pass
  statement, but coverage counters and reports are not exposed.
- **Default time scales for elements without one.** IEEE 1800-2023
  3.14.2.3 makes a design an error when some design elements have a time
  unit and precision (declared, inherited or from `` `timescale ``) and
  others do not. Celox instead warns, naming the elements without one,
  and each such element takes the default time unit and precision, as
  other simulators do. Strict mode (`strict = true` / `--strict`) or
  `mixed_timescale = "error"` in the
  [`[diagnostics]` table](./celox-toml.md#diagnostics) rejects the
  design; `mixed_timescale = "allow"` silences the warning.
- **Storage of interface subroutines called through a virtual interface.**
  A task or function of an interface called through a virtual interface
  (`vif.drive(req)`, 25.9) runs for the instance the handle denotes. Its
  static variables belong to that instance (13.3.2, 6.21), but are held by
  each scope that makes such calls, as the static storage of a subroutine
  called through an interface port is (see
  [Interfaces](#interface-restrictions)). Its formals belong to each call,
  also those of a static subroutine.
- **`$psprintf` is `$sformatf`.** The vendor system function `$psprintf`,
  which many simulators and older UVM code use, is accepted as `$sformatf`
  (21.3.3); diagnostics name it `$sformatf`.
- **UVM DPI functions are built in.** Imports with the C names of UVM's
  `src/dpi` functions bind to Celox's implementations instead of a user C
  function; see [DPI-C imports](#dpi-c-imports).

## Implementation-defined and unspecified behavior {#semantics-worth-knowing}

Where IEEE 1800-2023 leaves a choice to the implementation, or declares an
order unspecified or nondeterministic, Celox chooses as follows.
Portable code must not depend on these choices.

### Constant widths {#constant-widths}

IEEE 1800-2023 lets an implementation limit the width of a vector to no
less than 65536 bits (6.9.1, 7.4.1). A constant value, such as a parameter,
the concatenation that gives it or a part-select of it, may have up to
1048576 bits. A replication, a constant built bit by bit, and a value inside
a constant function call may have up to 65536 bits. A wider constant is
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
  error, as a constant one fails the analysis; Celox prints at most 4096
  decimal places and pads to at most 4096 characters, and a larger
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
- A 4-state rand property takes a 2-state value. The X and Z bits of a
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
- Array `shuffle` draws from the calling process's random stream.

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
- **Termination.** `final` procedures run once on `$finish`, `$fatal`, or an
  explicit `Simulation::finish()`; an idle `step()` or the end of
  `run_until()` does not terminate the reusable Rust simulation. No
  scheduled events run afterwards, so a nonblocking assignment made
  directly in a `final` procedure never applies its queued update. `$stop`
  in an `initial` block suspends the simulation (`is_stopped()` is set and
  the next call continues); in an `always` process it is reported as
  `RuntimeEvent::Stop`, as `$finish` is `RuntimeEvent::Finish`. Batch hosts
  without an interactive mode, such as the VPI host, end the run at
  `$stop`. A checkpoint cannot be taken while suspended
  (`CheckpointError::Stopped`).
- **Reports.** `$info`, `$warning`, `$error`, and `$fatal` — including the
  default `$error` of an assertion without a fail action — are delivered to
  the host as `RuntimeEvent::Report` with severity, message, file, line,
  scope, and simulation time. Only `$error` and `$fatal` fail a `celox`
  run; `$fatal` also ends the simulation.
- **Top-level modules.** A simulation runs the top module it names. A design
  with several top-level modules (23.3.1), such as a testbench's `hdl_top` and
  `hvl_top`, runs them together when Rust's `SimulatorBuilder::tops` or a
  repeated `celox run --top` names them,
  each as an instance of its own name under `$root`, with its ports
  unconnected. Hierarchical names, `%m`, waveform scopes and the VPI and
  reflection hierarchy then start at a top (`hdl_top.dut`, `$root.hdl_top.dut`):
  each top is a root scope, as `vpi_iterate(vpiModule, NULL)` returns them.
  Signals are named from a top, as `signal("hdl_top.clk")` or
  `child_signal(&[("hdl_top", 0)], "clk")`, and so are forced and live
  signals. The design's tops are the selected modules: `PreserveTopPorts`
  keeps the ports of each, `named_signals()` names each top's signals from
  it (`hdl_top.clk`), `named_hierarchies()` gives each top's hierarchy, and
  `named_hierarchy()` is `$root` with the tops as its children. Several tops
  require SystemVerilog sources only and take no
  parameter overrides.
- **Observable names.** Interfaces are expanded into the modules that use
  them: a member `m` of interface instance `h` becomes the signal `h$m`, an
  interface port at the top becomes ports such as `bus$data`, and a module
  with a generic `interface` port is copied per connected interface as
  `M$I`. Instance array elements are addressed by their declared index
  (`dut.u[3]`, `child_signal(&[("u", 3)], "y")`). A package variable is
  named `p::v` and a compilation-unit variable `$unit::v` in
  `sim.signal(…)`; hierarchical names can also reach `string` variables
  through the host API.
- **Heap and checkpoints.** Strings, class objects, and containers live in
  a heap collected between time slots and after each `tick`; a `string`,
  class handle, `process` handle, or virtual interface is stored like a
  32-bit integer. Core checkpoints (`checkpoint()` / `restore()`) preserve
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
Either language may be the top. Interfaces and `input` / `output` modports
cross the boundary in both directions.

Unsupported across the boundary: cross-language interface function calls
and function `import` / `export` modports; interface header functions with
statement bodies the contract cannot express (such as loops), header-
dependent formal or return widths, declaration casts whose target size
depends on interface parameters, and constant functions that cannot be
lowered symbolically. These remain available within one language.

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

Supported subset; everything else is rejected:

- `function` imports (not tasks, not `context`) declared in a module, in a
  package or at compilation-unit scope, called from procedural code:
  `initial` and `always` processes (`always @*` included), `always_ff`, and
  the subroutines they call. Calls in continuous assignments and
  `always_comb` are rejected.
- Up to 16 arguments, `input`, `output` or `inout`.
- Argument types `bit`, `logic` (as `svBit` / `svLogic`), `byte`,
  `shortint`, `int`, `longint` (signed or unsigned), `chandle`, `string`
  (as `const char *`), packed arrays of `bit`, `logic` or `reg`, and
  `integer` and `time` (as `svBitVecVal` / `svLogicVecVal` words); results
  of the types before the packed arrays, or `void` (35.5.5). `real`,
  `shortreal`, named types (typedefs, enumerations, structures), and
  unpacked or open arrays are rejected.
- An `output` or `inout` argument is a pointer to its C type, and to a
  `const char *` for a `string`. A string or packed array passed to the
  function is valid until it returns; a string it returns or writes to an
  output is copied when it returns.
- In two-state simulation, the unknown bits a function writes to a `logic`
  argument read as zero.
- `export "DPI-C"` declarations are accepted and checked (35.7) but no C
  code can call them, because only `context` imports may do so (35.5.3).
- UVM's C library functions are provided by Celox and shadow any user
  implementation of the same name. The
  `uvm_re_*` functions use the Rust `regex` crate, whose syntax differs
  from POSIX extended regular expressions in rare constructs;
  `uvm_dpi_get_next_arg_c` returns `celox` followed by the plusargs and
  `uvm_dpi_get_tool_name_c` returns `Celox`. The `uvm_hdl_*` and
  `uvm_polling_*` functions return 0 or `null`, so UVM reports those
  facilities as unavailable. UVM 1.2's `uvm_re_match`, `uvm_glob_to_re`,
  `uvm_dump_re_cache`, `uvm_dpi_regcomp`, `uvm_dpi_regexec` and
  `uvm_dpi_regfree` are provided too. Their reports (`UVM/DPI/REGEX_INV`,
  `UVM/DPI/REGEX_MAX`, `UVM/DPI/REGCOMP` and the cache message) go through
  the function the UVM package exports for them, `m__uvm_report_dpi`, as
  UVM's C library makes them, so they count as UVM reports; the text after
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



