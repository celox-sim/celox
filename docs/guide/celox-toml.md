# celox.toml

`celox.toml` is an optional Celox-specific configuration file placed alongside `Veryl.toml` at the project root. It extends the Veryl project with settings that are only relevant to simulation and testing.

## Why It Exists

`Veryl.toml`'s `[build] sources` list controls which directories are compiled for production. Test-only modules -- such as helper fixtures, mock peripherals, or reference models -- should not be included in those builds.

`celox.toml` lets you declare extra source directories that Celox loads **only** during simulation and type generation, keeping them out of the standard Veryl build.

## File Structure

Place `celox.toml` next to `Veryl.toml`:

```
my-project/
├── Veryl.toml
├── celox.toml          ← Celox configuration
├── src/
│   └── Adder.veryl     # production sources (listed in Veryl.toml)
└── test_veryl/
    └── Reg.veryl        # test-only sources (listed in celox.toml)
```

## Configuration Reference

```toml
exclude = ["test_veryl/broken.veryl", "src/legacy/**"]

[test]
sources = ["test_veryl"]

[simulation]
max_steps = 100000
```

### `exclude`

| Key | Type | Description |
|---|---|---|
| `exclude` | `string[]` | Glob patterns (relative to project root) for `.veryl` files to exclude from compilation and type generation. Applies to both production sources and test sources. |

Patterns use standard glob syntax (`*`, `**`, `?`, `[...]`). Path separators are always `/` regardless of platform.

### `[test]`

| Key | Type | Description |
|---|---|---|
| `test.sources` | `string[]` | Directories (relative to `celox.toml`) whose `.veryl` files are included in simulation and type generation. |

### `[simulation]`

| Key | Type | Default | Description |
|---|---|---|---|
| `simulation.max_steps` | `integer` | 100,000 | Default step budget for `waitUntil` and `waitForCycles`. A `SimulationTimeoutError` is thrown if the condition is not met within this many steps. Overridden per-call via `{ maxSteps }`. |

## Example

**`Veryl.toml`** — production build, only includes `src/`:

```toml
[project]
name    = "my_project"
version = "0.1.0"

[build]
clock_type = "posedge"
reset_type = "async_low"
sources    = ["src"]
```

**`celox.toml`** — additionally loads `test_veryl/` for simulation, excludes WIP files, and sets a project-wide step budget:

```toml
exclude = ["test_veryl/wip_*.veryl"]

[test]
sources = ["test_veryl"]

[simulation]
max_steps = 50000
```

**`test_veryl/Reg.veryl`** — a test-only module:

```veryl
module Reg (
    clk: input  clock,
    rst: input  reset,
    d:   input  logic<8>,
    q:   output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
        } else {
            q = d;
        }
    }
}
```

**`test/reg.test.ts`** — the test imports `Reg` just like any other module:

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Reg } from "../test_veryl/Reg.veryl";

describe("Reg", () => {
  test("captures input on rising edge", () => {
    const sim = Simulator.create(Reg);

    sim.dut.d = 0xABn;
    sim.tick();
    expect(sim.dut.q).toBe(0xABn);

    sim.dispose();
  });
});
```

The Vite plugin picks up `test_veryl/` automatically and generates type definitions for all modules declared there.

## Behavior

- If `celox.toml` does not exist, Celox uses only the sources listed in `Veryl.toml` and falls back to built-in defaults for all simulation settings.
- All test source directories are merged with the project sources at simulation time. Modules from both are available in the same namespace.
- The Vite plugin regenerates types for test sources on hot reload, just like for production sources.
- `[simulation]` settings apply to every `Simulation.fromProject` / `Simulation.create` call within the project. A per-call `{ maxSteps }` option always takes precedence over the `celox.toml` value.


### CLI SystemVerilog timing

For `celox run` and `celox vpi build`, set `timescale = "1ns/1fs"` at the
manifest root or in `[targets.NAME]`. `--timescale 1ns/1fs` overrides the selected
target, which overrides the root setting. The default remains `1ns/1ps`.

This sets the default time unit and precision for scopes without source timing
declarations. The configured precision also contributes to the global scheduler
resolution, so cocotb can schedule femtosecond clock periods even when an SV
module declares `1ns/1ps`. Explicit `timeunit`, `timeprecision`, and `` `timescale ``
settings retain their scope units and delay rounding. The global resolution is
the finest configured or declared precision. Values use magnitudes 1, 10, or 100
and units `s`, `ms`, `us`, `ns`, `ps`, or `fs`; precision cannot be coarser than unit.

### `[diagnostics]`

Override individual compilation diagnostics by their displayed code:

```toml
[diagnostics]
mixed_timescale = "warn"
inferred_latch = "warn"
exit_outside_program = "error"
```

`allow` suppresses the diagnostic, `warn` retains it without failing compilation,
and `error` rejects the design. Unlisted codes keep their default severity.
Existing Veryl and SystemVerilog warnings can be suppressed or promoted this way.
Use the exact code printed in the diagnostic; entries for codes not encountered
by a compilation have no effect.

Every diagnostic that has a defined fallback is a warning by default, so
existing designs and test suites compile with the fallback semantics below
and the diagnostic stays visible. Set `strict = true` at the manifest root
(or in a target), pass `--strict` to the CLI, or use the Rust
`.strict(true)` / JavaScript `strict: true` option to promote all of them
back to errors. An explicit per-code level always wins over strict mode.

Recoverable errors support relaxation only when the compiler defines a fallback.
`mixed_timescale` is one such diagnostic: IEEE 1800-2023 §3.14.2.3 requires
consistent explicit time units and precisions. Its default is `warn` (`error`
in strict mode). With `warn`
or `allow`, undeclared settings use Celox's defaults (1ns unit, 1ps precision),
explicit settings remain effective, and the global precision includes default
scopes. The error names the design elements without a unit or precision, and
`warn` reports one warning for each, naming it. Syntax errors, unsupported constructs and invalid precision declarations
remain errors.

`interface_array_query` defaults to `warn` (`error` in strict mode). Set it to `allow` to enable it silently, or keep `warn` to enable
`$size` on fixed-size interface arrays and index-selected subarrays as a compatibility
extension. IEEE 1800-2023 §20.7 specifies queries on data arrays; interface
instances are not data objects. The optional dimension must be a known integer constant. This
extension does not give interface handles a packed width or enable other queries.

```toml
[diagnostics]
interface_array_query = "warn"
```

The CLI uses the same policy: `celox run top.sv --top Top --diagnostic interface_array_query=warn`.

`duplicate_module` defaults to `warn` (`error` in strict mode): a design declares each module once. With
`warn` or `allow`, the first declaration of a module in source order is the
module and the later declarations of the same name are left out, as tools that
only warn about them do. Interfaces, programs and packages declared twice
remain errors.

`interface_array_parameter` also defaults to `warn` (`error` in strict mode). Set it to `warn` or `allow`
for the compatibility shorthand `bus.WIDTH` when `bus` is an interface array.
It reads the value parameter from the first element in declaration order,
including arrays with nonzero bounds. Signals and type parameters still require
an explicit element selection. The same setting works in `[diagnostics]` and
with `--diagnostic interface_array_parameter=warn`.


`multiple_always_ff_drivers` defaults to `warn` (`error` in strict mode) for shared memories written by
an `always_ff` and another edge process (IEEE 1800-2023 §9.2.2.4). With `warn`
or `allow`, Celox stages their nonblocking writes in sparse next-state storage
and commits dirty chunks. Ordinary `always` procedures can share a memory
without this diagnostic. Simultaneous
writes to the same bits from different processes have no portable ordering.

`inferred_latch` warns about retained assignments in SystemVerilog and Veryl
`always_comb` by default (IEEE 1800-2023 §9.2.2.2 asks tools to warn when
latched behavior can be inferred) and simulates the latch with a
source-located warning; in strict mode it rejects the design instead.
Set it to `allow` to simulate it silently. Ordinary `always @*`
and explicit `always_latch` do not require this relaxation. Nonblocking latch
assignments and unsupported combinational feedback remain errors.
The Rust `.allow_inferred_latches(true)` and JavaScript `allowInferredLatches: true`
options provide `allow` only when this diagnostic has no explicit setting;
an explicit `error`, `warn` or `allow` takes precedence.

Project simulator constructors load this table alongside `Veryl.toml`. Native
compilation also supports standalone SystemVerilog projects:

```sh
celox run bus.sv top.sv --top Top --diagnostic mixed_timescale=warn
celox vpi build --project ./celox.toml --target compat --diagnostic exit_outside_program=allow
```

Both commands use the project discovery and path rules in the
[CLI guide](./cocotb.md). With explicit sources, use `--project FILE` to load
configuration. With no inputs, or with `--target NAME`, they discover `celox.toml`
from the current directory and its ancestors. A selected target can override
individual codes without replacing the entire policy:

```toml
sources = ["bus.sv", "top.sv"]
top = "Top"

[diagnostics]
mixed_timescale = "error"

[targets.compat.diagnostics]
mixed_timescale = "warn"
```

The order is common diagnostics → selected target → CLI overrides. Repeat
`--diagnostic CODE=LEVEL` to override multiple codes; the last assignment to a
code wins. `celox run` and `celox vpi build` print retained compilation warnings.
Diagnostic severity does not change optimizer settings or runtime error handling.

### `[compatibility]`

Opt-ins that change simulation behavior for compatibility with other
tools, grouped separately from diagnostic severities:

```toml
[compatibility]
legacy_empty_reductions = true
shared_always_temporary = true

[targets.legacy.compatibility]
legacy_empty_reductions = false
```

`legacy_empty_reductions` returns zero for every empty SystemVerilog
array reduction instead of each operator's identity (product one,
bitwise AND all ones, and so on; IEEE 1800-2023 §7.12.3). The CLI flag
is `--legacy-empty-reductions`; the Rust spelling is
`SimulatorBuilder::legacy_empty_reductions(true)`.

`shared_always_temporary` treats a variable written by several
`always @*` blocks as a temporary of each block. By default Celox keeps
IEEE 1800-2023 semantics for such a variable, such as a shared loop
index: each block waits on the variables it reads, the shared one
included (§9.4.2.2), and writes notify other blocks that are waiting.
With the opt-in, Celox instead makes the variable a temporary of each
block when only `always @*` blocks write it, each writes it before it
reads it on every path, and nothing else reads it. Its writes then wake
no other block, as in Icarus Verilog, and the variable itself keeps its
initial value. This deviates from the standard, so it is off by
default; when it applies, Celox reports each variable and its blocks
as a warning. It is not a diagnostic code: a level for it under
`[diagnostics]` has no effect. The CLI flag is
`--shared-always-temporary`, and the Rust spelling is
`SimulatorBuilder::shared_always_temporary(true)`.

The order is common compatibility → selected target → CLI flags.
Project simulator constructors read the same table from `celox.toml`.

## CLI optimization and signal access

`celox run` and `celox vpi build` accept these project/target settings:

```toml
sources = ["top.sv"]
top = "Top"
opt_level = "O2"
live_signals = ["Top.uut.debug_count"]
force_signals = ["Top.uut.state"]

[targets.observe]
opt_level = "O1"
force_signals = []
```

The default optimization level is `O2` for `celox run` and `O1` for `celox vpi build`. Signal lists default to empty. `O2` may remove internal
computations that do not affect design outputs; `live_signals` retains the
listed objects for external observation. `force_signals` also implies liveness
and emits external override controls before symbolic evaluation and SIR
optimization, so blocking assignments and downstream expressions observe the
forced value. It does not switch the whole design to `O0`. Force permission is
retained in the standalone native image; undeclared force requests fail.

Use exact hierarchy paths including the top module, as reported by VPI (for
example `Top.uut.s_axis.tdata`). Missing or ambiguous paths fail compilation;
wildcards are not expanded. Explicit force currently supports whole packed
SystemVerilog variables, including owned interface members. For an interface member, select its owning
variable rather than a child modport alias; structured port alias targets are
rejected. Unpacked arrays, clock
and reset event signals, and Veryl/mixed-language force targets are rejected.
Observation roots support both source languages.

```sh
celox vpi build --project celox.toml -O O2 \
  --live-signal Top.uut.debug_count --force-signal Top.uut.state -o dut-sim
celox run --project celox.toml --target observe --no-force-signals
```

Priority is common settings → selected target → CLI. Each signal list replaces
the preceding list as a whole; an omitted target list inherits the common list,
and `[]` clears it. Repeat `--live-signal` or `--force-signal` for multiple paths.
`--no-live-signals` and `--no-force-signals` clear inherited lists and conflict
with the corresponding path options. These settings apply to the two CLI
compilation commands; JavaScript simulator options are configured separately.

## CLI top parameter overrides

Both `run` and `vpi build` accept repeatable `-G NAME=VALUE` or
`--parameter NAME=VALUE` (`--param` is an alias). These override numeric
parameters of the selected top module in SystemVerilog or Veryl, before
elaboration, without modifying source files.

```sh
celox vpi build --project celox.toml -G ID_EN=1 -G DEST_EN=1 -o dut-sim
```

```toml
sources = ["top.sv"]
top = "Top"

[parameters]
ID_EN = 1
DEST_EN = 1
DATA_W = 8

[targets.wide.parameters]
DATA_W = 64
```

Common parameters are merged with the selected target's parameters, followed
by CLI assignments. The last assignment to each name wins; unrelated entries
are inherited. Values are unsigned 64-bit integers: decimal, `0x` hexadecimal,
`0o` octal, or `0b` binary. Digit separators such as `0x12_34` are accepted.
TOML values can be integers or strings (`DATA_W = "0x40"`); use strings for
values above TOML's signed 64-bit integer range. Negative numbers, HDL
expressions/literals, strings as HDL values, and type parameters are not
supported by this interface. Declared HDL parameter types still determine
conversion and truncation. Invalid assignments and unknown parameter names
produce errors.
