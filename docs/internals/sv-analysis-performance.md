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

## Parameter environments and scope snapshots

The next follow-up starts from master `064b9541f`. Module parameter collection
now carries a parameter-only value/type prefix alongside the inherited
environment. It binds each appended parameter once rather than rebuilding all
preceding parameters for each declaration and initializer. The prefix keeps
its existing precedence over inherited values and type markers. It is local to
one collection pass; later passes rebuild it with the current aliases and
specialization overrides. Scalar declaration headers avoid constructing range
environments. Packed ranges and aliases retain the existing range-lowering path.

Modules without a direct enum declaration skip enum-member collection. Modules
with enums retain the parameter and alias refreshes needed for interleaved enum
and parameter dependencies. Instance, process, and subroutine collectors also
skip building body contexts for parameter declarations, which produce no bodies;
those declarations still undergo constant collection and validation.

Generated item snapshots and packed-dimension contexts share numeric environments
and parameter-expression tables through `Arc`. A mutable scope detaches a shared
table on its first write. Other scopes keep their previous values. Parameter
dimension tables are computed once per completed scope and shared among its
items; binding generate-local parameters or removing a shadowed loop parameter
refreshes that table before emitting items.

These optimizations preserve the width/sign rules for value parameters
(IEEE 1800-2023 6.20.2, **Value parameters**) and local-binding precedence
(23.9, **Scope rules**). Analyzer regressions compare incremental collection
with rebuilding across inherited values, overrides, signed aliases, casts,
unknown literals, and forward references. They check repeated specialization,
snapshot mutation isolation, and exactly 128 prefix-binding attempts for 128
simple declarations, without a wall-clock assertion.

The existing `type_queries --parameters` probe checks the final constant value
or generated signal width. Using the same profile, inputs, machine, and three-run
median procedure as above, AST construction takes:

| Workload | Count | AST before (ms) | AST after (ms) |
| --- | ---: | ---: | ---: |
| declaration-order parameters | 16 | 32.751 | 3.678 |
| declaration-order parameters | 64 | 306.720 | 17.053 |
| declaration-order parameters | 256 | 6,760.006 | 125.544 |
| reverse generate dependencies | 16 | 28.978 | 8.413 |
| reverse generate dependencies | 64 | 420.373 | 71.882 |
| reverse generate dependencies | 256 | 7,435.463 | 863.795 |

At 256 entries, these are about 54x and 8.6x improvements in AST construction.
The after run parses the two inputs in about 26/30 ms, and converts AST to IR in
about 0.27/0.02 ms. The measurements exclude backend compilation and simulation.
Shared-machine load varies between runs; these numbers describe these inputs,
not a universal speedup. Overall AST construction still shows superlinear growth.

## Generate dependency scheduling

The next follow-up starts from master `94701163f`, after parameter-environment
and snapshot reuse. Signal dimensions and generate-local parameters are now
scheduled by one local dependency graph. Each declaration records its remaining
prerequisites and reverse edges. Only local names form edges; inherited names
retain the previous lookup behavior. Completing a binding releases its outgoing
edges once, rather than rebuilding the unresolved-name set and scanning both
pending lists after every declaration.

A minimum-index heap preserves the previous priority: ready signals first,
then ready parameters, with declaration order within each group. Newly ready
signals take priority over parameters already in the queue. Edges are released
only after successful binding, so an error in a ready declaration is still
reported before a cycle in the remaining graph. The readiness work is expected
O(V + E + V log V) for V declarations and E local dependency edges. Payloads
stay in indexed optional slots, avoiding repeated vector removal/shifting.

Parameter lowering temporarily appends a declaration's siblings and drains that
suffix, retaining only the scheduled parameter. This preserves its preceding
bindings without cloning the inherited parameter vector. The dependency
extraction, local-name masking, parameter typing, and cycle diagnostics remain
the same. Generated names and shadowed-name sets now share immutable snapshots;
mutating a child or a function's formal bindings detaches the corresponding table.
Empty inherited function/return-type tables skip generated-name rewriting, whose
result would also be empty.

Analyzer regressions compare all 512 directed three-node graphs with the old
priority order, including self-edges and cycles. They cover signal precedence,
external names, a 4,096-node reverse chain with one release per edge, ready-error
precedence over remaining cycles, grouped declarations shadowing outer values,
and isolation of generated name snapshots. These are tests of analyzer
representation, scheduling, and diagnostics; no shared language cases are added.

The `generate_dependencies` probe measures one generate block containing fixed
width signals, checks every qualified signal name and width, and separates parse,
AST construction, and IR conversion. Its `--scheduler` mode compiles the production
scheduler directly and compares it with the former readiness loop on independent
nodes, reverse chains, and fanout. That mode excludes syntax parsing, declaration
binding, scoped type expansion, and simulation. Both modes use medians of three
fresh runs and impose no timing thresholds.

```sh
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example generate_dependencies
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example generate_dependencies -- --scheduler 128 512 2048
```

Measurements use the same optimized profile and three-run median procedure
above. In the same process, isolated readiness scheduling takes:

| Graph | Vertices | Edges | Legacy (ms) | Graph (ms) |
| --- | ---: | ---: | ---: | ---: |
| independent | 128 | 0 | 0.396 | 0.017 |
| independent | 512 | 0 | 9.321 | 0.021 |
| independent | 2,048 | 0 | 100.391 | 0.161 |
| reverse chain | 128 | 127 | 0.532 | 0.009 |
| reverse chain | 512 | 511 | 8.675 | 0.031 |
| reverse chain | 2,048 | 2,047 | 153.115 | 0.206 |
| fanout | 128 | 127 | 0.398 | 0.010 |
| fanout | 512 | 511 | 6.920 | 0.028 |
| fanout | 2,048 | 2,047 | 110.467 | 0.155 |

These numbers measure scheduling only, including graph construction. The legacy
probe retains original-index slots while simulating the former readiness scan;
it does not include the production payloads' larger vector shifts or their binding
work. Every produced order is checked against the expected priority.

Fresh AST measurements for one block of independent generated signals:

| Signals | AST before (ms) | AST after (ms) |
| --- | ---: | ---: |
| 64 | 105.607 | 68.618 |
| 256 | 1,294.453 | 739.714 |
| 1,024 | 18,901.242 | 14,528.035 |

The 1,024-signal inputs parsed in about 162/156 ms before/after. IR conversion
took about 0.69/0.50 ms. Thus the isolated scheduling gain does not imply linear
AST construction: repeated scoped type/name expansion still dominates this input.
The machine was shared and timings varied substantially between separate runs.

For the existing 256-entry reverse-parameter probe, the later before/after AST
samples were 1,044.361/1,495.835 ms, with parsing 35.771/60.640 ms. The ordinary
256-parameter samples were 143.745/222.950 ms. These noisier samples do not
establish an overall parameter-chain speedup; prefix/range rebuilding remains
outside this scheduler change. The counters and order comparisons establish the
bounded scheduling work independently of those wall-clock variations.

## Remaining boundaries

Type-parameter substitutions and package inlining rewrite source text and still
require parsing the rewritten source. Nested module declarations retain the
syntax-walk fallback; ordinary top-level module lookup avoids unrelated bodies.
Resolving the parser's source root can still visit its description list.

Type-alias tables are still copied in some scope paths. Generated names and
shadowed-name sets copy on mutation rather than for every item snapshot. Shared numeric/parameter-expression tables copy their contents when a
shared scope mutates them, and some borrowed-map constructors still copy an
environment. Many dependent parameters, many functions
inside generated scopes, and `$bits`/`$size` queries during declaration lowering
or in numeric cast targets can have different scaling from the flat probes.
Those preliminary queries still discover enclosing declarations by walking
syntax. Query contexts still clone alias/function-type tables. Parameter ranges and
modules with enums still rebuild some environments, generated scopes still
rebuild parameter prefixes. Building scoped dimension views still rebinds
visible generated names for each item, and those views are created in collectors
even for some items that produce no output there. Applying parameter dimensions and materializing scoped literals also
contribute work that grows with the number of visible parameters.
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
