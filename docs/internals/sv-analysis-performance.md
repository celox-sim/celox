# SystemVerilog analyzer scaling

The scaling probe separates `sv-parser`, analyzer AST construction, and AST-to-IR
conversion. It also compares parsing a multi-module file for each selected
module with reusing one `ParsedSource`. This measures frontend work, without
backend compilation or simulation.

```sh
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example scaling
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example scaling -- --hierarchy
```

Optional integer arguments replace the default counts. Flat designs default to
128, 512, and 2,048 signals/processes; hierarchy measurements default to 8, 32,
and 128 modules. Each reported phase is the median of three fresh runs. Inputs
and output counts are checked; wall-clock thresholds are deliberately absent.
The [probe source](https://github.com/celox-sim/celox/blob/740f969402818f7d2552d0991d5c96247c456533/crates/celox-sv-analyzer/examples/scaling.rs) contains
the generators and measurement boundaries.

## Changed algorithms

Previously, each declaration or process copied the module's complete variable
dimension map. With S signals and B bodies/items, this contributed O(S * B)
work even when a body referenced one signal. Procedural local declarations also
copied the entire signedness map through `Arc::make_mut`.

Variable dimensions and signedness now use a shared module table plus a small
scope overlay. A scope clone shares both tables; mutation copies local changes
only when an overlay is still shared. Insertion, shadowing, and removal affect
the overlay. Removed base names have
tombstones, and iteration yields each visible name once. Module construction
fills an exclusively owned base directly. Ordinary module items also reuse the
function table, while generated scopes still rewrite definition-site bindings
when they introduce aliases or shadowed names.

Assignment coercion now queries signedness for identifiers used in the
expression, rather than collecting all signal names for every assignment.
Parameter-name collision checks use name sets. Connected nets are collected
once from instance actuals, replacing a scan of every connection for every net.
The set preserves the previous connection forms for which driver validation
is deferred to the frontend adapter.

The SV frontend previously parsed whole files for interfaces, packages, module
names, type-parameter rewrites, package inlining, and each specialization. It
also copied the entire source text into every module record. It now shares one
`ParsedSource` per file through `Rc`. Names, interfaces, and top-level module
indices are collected once. Interfaces from the current file and other files
are queried through borrowed tables, with the existing local precedence.
Numeric specializations lower fresh ASTs from the shared syntax tree.

The syntax cache lives for one frontend build, rather than globally. It retains
the original CST until that build finishes, trading that linear storage for
repeated parsing and per-module source copies. `sv-parser`'s tree is confined
to its parsing thread. Generated simulator execution remains independent of
this syntax cache.

## Measurements

Measurements on an AMD Ryzen 7 9800X3D under WSL, Rust 1.99.0, using the
`heliodor-dev` profile (optimized, no LTO). The baseline is revision
`c802d5b64`, with the same assign/comb input generators and profile.
Times are milliseconds and are illustrative measurements on a shared machine,
not acceptance thresholds or a claim about every SV workload.

| Workload | Signals | AST before | AST after |
| --- | ---: | ---: | ---: |
| continuous assign | 128 | 160.782 | 18.659 |
| continuous assign | 512 | 2,380.909 | 62.772 |
| continuous assign | 2,048 | 39,361.071 | 280.932 |
| always_comb | 128 | 203.982 | 16.932 |
| always_comb | 512 | 2,884.785 | 75.250 |
| always_comb | 2,048 | 42,455.450 | 298.759 |

At 2,048 signals, AST construction improves by about 140x for continuous
assignments and 142x for `always_comb`. Parsing still takes about 497–524 ms
for these inputs, and IR conversion takes about 1–3 ms. The AST measurements
exclude those phases and should not be interpreted as end-to-end speedups.

Additional post-change AST measurements:

| Workload | 128 items | 512 items | 2,048 items |
| --- | ---: | ---: | ---: |
| always_comb with a local variable | 37.736 | 150.626 | 665.813 |
| always_ff | 24.611 | 88.814 | 369.507 |
| loop generate | 62.650 | 241.653 | 1,018.967 |

The hierarchy probe analyzes every module of one file. Both modes use the
post-change analyzer, so this comparison isolates syntax reuse and includes the
initial parse in the reused mode. It does not measure a complete simulator
build or the old frontend's additional metadata/rewrite parses.

| Modules | Parse separately per module | Reuse parsed source |
| --- | ---: | ---: |
| 8 | 37.387 | 5.759 |
| 32 | 531.142 | 26.507 |
| 128 | 8,819.291 | 109.228 |

## Type-query scope reuse

The follow-up removes repeated declaration discovery from `$bits`/`$size`
expressions once a module's ports, signals, and function types have been
collected. Generated and procedural scopes overlay that completed metadata.
Preliminary parameter, function, and range lowering keeps the completion flag
false; an unresolved query still falls back to syntax discovery.

Queries use unexpanded function calls, so `$size(f())` retains the first packed
return dimension rather than the width of an inlined body. Operand signedness
and implicit parameter widths use per-name lookups instead of reconstructing
the complete parameter type table. An unqualified typedef parsed as an
expression can use the alias table directly when no visible value shadows it.

This follows IEEE 1800-2023 20.6.2, **Expression size system function**, and
20.7, **Array query functions**: fixed-size queries use operand types, and
`$bits` does not evaluate the enclosed expression. Completed-scope metadata
also preserves the local binding required by 23.9, **Scope rules**. For
example, a generate-local `localparam x = 1` hides an eight-bit module input
`x`; its implied packed range follows 6.20.2, **Value parameters**. Its
`$bits` and `$size` results are 32 in Celox. The old declaration rebuild could
reintroduce the outer eight-bit signal. A sized unknown literal similarly
retains its declared literal width without requiring a numeric value.

The shared `generate::size_queries_use_generate_local_parameter_types` case
checks both known and unknown local parameters through the backend harness and
Verilator/Icarus. Analyzer tests compare completed-scope results with discovery
for arrays, selects, structures, aliases, and function returns; they also assert
that these queries do not rewalk declarations. Separate tests cover preliminary
function scopes, typedef shadowing, and repeated parameter specializations.

The `crates/celox-sv-analyzer/examples/type_queries.rs` probe uses the same
three-run, phase-separated measurements as `scaling`. Its default counts are
16, 64, and 256 queries. `--parameters` selects declaration-order parameters
and reverse-ordered generate-local dependencies instead.

```sh
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example type_queries
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example type_queries -- --parameters 16 64
```

The follow-up's baseline is `3665f501e`, after scope sharing and syntax reuse.
It uses the same `$bits`, `$size`, selected-value, and function-value inputs.
The after measurements include the intervening correctness fixes in `6c5b09891`
and use the same machine, profile, and three-run median procedure above.

| Query | Count | AST before (ms) | AST after (ms) |
| --- | ---: | ---: | ---: |
| `$bits(a)` | 16 | 10.644 | 2.290 |
| `$bits(a)` | 64 | 135.854 | 8.197 |
| `$bits(a)` | 256 | 2,407.279 | 29.080 |
| `$size(a)` | 16 | 10.729 | 1.900 |
| `$size(a)` | 64 | 138.075 | 6.965 |
| `$size(a)` | 256 | 3,491.439 | 26.993 |
| `$bits(a[0])` | 16 | 13.030 | 2.673 |
| `$bits(a[0])` | 64 | 167.299 | 9.769 |
| `$bits(a[0])` | 256 | 2,574.959 | 39.533 |
| `$bits(f(a))` | 16 | 14.360 | 3.023 |
| `$bits(f(a))` | 64 | 169.062 | 9.857 |
| `$bits(f(a))` | 256 | 2,483.456 | 38.064 |

The completed-scope paths avoid declaration rewalks even when the module has
many unrelated signals. At 256 queries, parsing still takes about 47–71 ms
for these four inputs; AST-to-IR conversion takes less than 0.2 ms. These AST
improvements exclude parsing and backend work. The added typedef workload
`$bits(byte_t)` takes 2.111/6.955/26.921 ms of AST construction at 16/64/256
queries; no before measurement is recorded for that workload.

The parameter probe still shows quadratic growth: before this follow-up,
16/64 declaration-order parameters took 13.785/201.113 ms of AST construction,
and reverse generate dependencies took 15.630/182.504 ms. After measurements
are 13.705/186.642 ms and 15.865/179.495 ms, respectively. This change does not
replace their dependency scheduling or repeated environment construction.

## Remaining boundaries

Type-parameter substitutions and package inlining rewrite source text and still
require parsing the rewritten source. Nested module declarations retain the
syntax-walk fallback; ordinary top-level module lookup avoids unrelated bodies.
Resolving the parser's source root can still visit its description list.

Constant environments, type-alias tables, and generated parameter metadata are
still copied in some scope paths. Many dependent parameters, many functions
inside generated scopes, and `$bits`/`$size` queries during declaration lowering
or in numeric cast targets can have different scaling from the flat probes.
Those preliminary queries still discover enclosing declarations by walking
syntax. Query contexts also clone some constant/alias/function-type tables.
This change does not establish linear scaling for those workloads. The probes
provide reproducible baselines for further optimization.

## Validation

Scope-overlay and reusable-source regressions belong to the analyzer because
they check internal representation, cache isolation, and its API. They cover
shadow/remove/restore, repeated numeric specializations and constant functions,
local/external positional interfaces, type-parameter and package rewrites,
nested and escaped names, duplicate modules, and non-ANSI rejection.

The existing SystemVerilog integration suite exercises native, Cranelift, and
Wasm consumers, including parallel and four-state cases. DPI and unpacked-array
regressions also run:

```sh
cargo fmt --all -- --check
cargo clippy --locked -p celox-sv-analyzer -p celox-frontend-sv --all-targets --all-features -- -D warnings
cargo test --locked -p celox-sv-analyzer -p celox-frontend-sv --all-features
cargo test --locked -p celox --features sv-dpi --test systemverilog --test sv_unpacked_array --test sv_dpi
cargo doc --locked -p celox-sv-analyzer -p celox-frontend-sv --no-deps
pnpm docs:build
```

The type-query follow-up also validates the suite catalogue, including the
retained-report integration tests, and the new shared case with both available
independent simulators. Add each new case's observed results to both retained
reports under `crates/celox-test-suite/verification/sv`:

```sh
cargo test --locked -p celox-test-suite --features verilator,icarus
cargo run --locked -p celox-test-suite --features verilator,icarus --bin verify-sv-verilator -- --filter generate::size_queries_use_generate_local_parameter_types
cargo run --locked -p celox-test-suite --features verilator,icarus --bin verify-sv-icarus -- --filter generate::size_queries_use_generate_local_parameter_types
```
