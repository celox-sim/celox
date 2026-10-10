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

## Removing repeated generate-scope work

The scope follow-up starts from `a7fbb309e`, which contains the dependency scheduler
and master `a7386fb34`. Three additional sources of quadratic work are removed:

- Collectors cache dimension and parameter-literal views by the identities of all
  five immutable scope snapshots: numeric environment, literals, names, shadowed
  names, and parameter dimensions. A collector fixes its inherited inputs for the
  cache lifetime and retains borrowers of every keyed item, so addresses cannot be
  recycled and changed child snapshots cannot reuse their parent's view. Nested
  scopes can interrupt siblings without rebuilding their views. Process, instance,
  and subroutine collectors request views only for applicable syntax.
- Validation tracks enter/leave events and generate depth instead of comparing
  every syntax node with a vector of all generated descendants. Generated items
  still undergo validation with their own lexical environment. Recursive item
  validation borrows the cached dimensions instead of copying the entire numeric
  environment and literal table again.
- Constant-bound evaluation substitutes typed values by looking up the identifiers
  actually present in the expression. It no longer rebuilds the full parameter
  type table, including scanning unrelated variable metadata, for each bound.
  Protecting a shadowed outer parameter's type also uses a direct lookup.

The cache is local to each collector; it neither crosses analyses nor reuses views
with different inherited inputs. Scope binding and shadowing follow IEEE 1800-2023
23.9. Analyzer regressions check a single materialization for 16, 64, and 256
siblings; interrupted/nested scopes; independent mutation of all five snapshot
fields; different inherited literals; validation around nested generate bodies;
and typed substitution across every constant-expression variant. A 4,096-parameter
unrelated environment does not increase the number of expression type lookups.
The existing backend suite checks language behavior; no portable cases are added.

Before merging the later declaration-collection refactor, the same optimized
profile and three-run median procedure give these complete AST construction
measurements for a block of independent signals (`bb0b2c124`):

| Signals | AST before (ms) | AST after (ms) | Parse after (ms) |
| --- | ---: | ---: | ---: |
| 128 | 250.620 | 56.110 | 19.905 |
| 256 | 827.680 | 117.491 | 39.106 |
| 512 | 3,202.770 | 195.818 | 59.799 |
| 1,024 | 13,650.542 | 393.066 | 107.448 |
| 2,048 | — | 1,150.577 | 247.708 |
| 4,096 | — | 2,063.718 | 734.129 |

At 1,024 signals, parsing before was 145.591 ms and IR conversion before/after was
0.468/0.405 ms. AST construction is about 35x faster. Expanding the after input
from 128 to 4,096 signals (32x) increases AST time about 37x, replacing the earlier
fourfold cost for each doubling. The probe's flat declaration path retains sorting
and heap scheduling, so its expected work includes O(N log N); it is not strictly
O(N). Shared-machine load affects timings, especially the larger inputs.

The `--assignments` mode also emits one constant continuous assignment per signal,
checks the number of produced processes, and checks every signal name/width:

```sh
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example generate_dependencies -- 128 256 512 1024 2048 4096
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example generate_dependencies -- --assignments 128 256 512 1024 2048 4096
```

| Signals and assignments | AST after (ms) | Parse after (ms) | IR after (ms) |
| --- | ---: | ---: | ---: |
| 128 | 59.735 | 32.774 | 0.147 |
| 256 | 134.387 | 64.576 | 0.341 |
| 512 | 260.489 | 142.033 | 0.653 |
| 1,024 | 627.613 | 343.221 | 1.458 |
| 2,048 | 1,372.160 | 701.086 | 2.902 |
| 4,096 | 4,396.238 | 2,372.087 | 10.156 |

This is an after-only workload, with no old assignment-mode timing. The final
sample has a corresponding increase in parser and IR time; the source inspection
and operation-count regressions establish the eliminated repeated work without
using wall-clock thresholds. These probes exclude backend compilation and simulation.

The final combined tree also includes master `2d4d82603`, adapting the collectors
to its shared module/package declaration representation. The `always_comb` path
now also passes shared numeric and literal snapshots through its collectors,
instead of copying their contents per process. Its body-local writes detach those
tables. A 4,096-entry snapshot regression checks the shared identities and isolated
mutations. This preserves the previous environment replacements, including their
handling of protected outer bindings.

The final source was measured in all three modes. `--always` emits one
`always_comb` per signal and checks the same names, widths and process counts:

```sh
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example generate_dependencies -- --always 128 256 512 1024 2048 4096
```

| Signals | AST: declarations (ms) | AST: assignments (ms) | AST: always_comb (ms) |
| --- | ---: | ---: | ---: |
| 128 | 36.819 | 41.625 | 42.006 |
| 256 | 74.010 | 83.129 | 87.931 |
| 512 | 148.488 | 166.917 | 184.854 |
| 1,024 | 318.319 | 332.597 | 395.054 |
| 2,048 | 659.970 | 765.664 | 699.383 |
| 4,096 | 1,371.800 | 1,413.936 | 1,350.127 |

Expanding these inputs 32x (128→4,096) increases AST time about 37x, 34x and 32x,
respectively. The final 1,024-signal declaration sample is about 43x faster than
`a7fbb309e`. These flat probes retain O(N log N) scheduling/sorting and do not
establish strictly linear behavior for every supported input.

The other phases, recorded in the same final runs:

| Signals | Parse: declarations (ms) | IR: declarations (ms) | Parse: assignments (ms) | IR: assignments (ms) | Parse: always_comb (ms) | IR: always_comb (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 128 | 10.701 | 0.042 | 20.621 | 0.094 | 25.811 | 0.067 |
| 256 | 20.760 | 0.088 | 39.838 | 0.163 | 45.085 | 0.139 |
| 512 | 39.302 | 0.160 | 81.414 | 0.323 | 111.659 | 0.246 |
| 1,024 | 84.659 | 0.329 | 173.564 | 0.652 | 234.837 | 0.488 |
| 2,048 | 197.389 | 0.548 | 376.525 | 1.676 | 417.511 | 0.842 |
| 4,096 | 407.947 | 1.035 | 808.031 | 3.173 | 794.149 | 1.696 |

A preceding combined-tree run, before the shared `always_comb` handoff, measured
1,024/4,096 plain declarations at 829.758/3,580.860 ms and continuous assignments
at 916.015/3,584.016 ms. Its declaration parsing was 234.480/937.956 ms and IR was
0.537/6.140 ms. This variation in paths unaffected by that handoff demonstrates
shared-machine timing variability. Source inspection and operation-count
regressions establish the eliminated repeated work; no wall-clock threshold is
part of the tests. All three probes exclude backend compilation and simulation.

Master `586bc7dc7` was subsequently synchronized, including its select-index
width fix and CI test-selection updates. The probes contain no select indices,
and the implementation paths they exercise are unchanged by that synchronization;
the final timing samples above are retained. Regression validation covers the
combined tree and the expanded 222-case external SystemVerilog catalogue.

## Reusing numeric parameter prefixes and range environments

A further follow-up synchronizes master `911903723`, preserving the remote PR's
package-scope reconciliation. Ordinary numeric parameter headers now borrow their
width-evaluation environment. The two previous copied maps have identical contents
when the header has no parameter assignments, the declaration-order prefix has no
four-state literals, and the inherited map is the same immutable object that
initialized the prefix. `ParameterEnvironment` retains a borrowed reference to that
object: the address check cannot match an object whose contents changed or whose
storage was recycled. Different inherited inputs, four-state prefixes and headers
containing assignments retain the previous copy, seed and mask behavior.

Generate-local single-parameter declarations reuse the prefix and the complete
parameter-type table while they remain consecutive in the dependency priority
order. A newly ready signal takes priority and ends the run before publishing new
type metadata. A grouped declaration also ends the run before temporarily binding
siblings. The next run rebuilds from that updated scope. Type-table updates follow
the same successful bindings as the former whole-environment discovery. Imported
parameter types remain present in the complete table, independently of the prefix
used for declaration-order resolution.

Owning-crate regressions check zero width-environment copies for 16/64/256 numeric
headers, equivalence with the deliberately selected copied path for aliases,
overrides and inherited values, four-state fallback, linear prefix binding counts
for 16/64/256 reverse generate dependencies, signal interruptions before size
queries, and cached/rebuilt type parity for imports and four-state/wide values.
Grouped declaration and diagnostic-priority regressions remain in place.
No shared executable cases are added.

The existing phase-separated `type_queries` probe adds `--ranged-parameters`,
which uses `logic [31:0]` headers. It checks final parameter values and the generated
signal width, retains three-run medians, and excludes backend compilation and
simulation:

```sh
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example type_queries -- --ranged-parameters 32 128 512
cargo run --locked -p celox-sv-analyzer --profile heliodor-dev --example type_queries -- --parameters 32 128 512
```

| Workload | Parameters | AST before (ms) | AST after (ms) |
| --- | ---: | ---: | ---: |
| ranged module declarations | 32 | 72.043 | 12.565 |
| ranged module declarations | 128 | 274.335 | 48.404 |
| ranged module declarations | 512 | 6,050.021 | 190.586 |
| ranged reverse generate dependencies | 32 | 169.318 | 13.736 |
| ranged reverse generate dependencies | 128 | 905.534 | 52.456 |
| ranged reverse generate dependencies | 512 | 13,741.234 | 215.087 |
| int module declarations | 32 | 93.254 | 8.580 |
| int module declarations | 128 | 504.828 | 47.811 |
| int module declarations | 512 | 2,061.696 | 477.861 |
| int reverse generate dependencies | 32 | 91.649 | 8.664 |
| int reverse generate dependencies | 128 | 527.482 | 31.031 |
| int reverse generate dependencies | 512 | 6,736.087 | 124.536 |

For the ranged inputs, increasing the after input sixteenfold (32→512) increases
AST time about 15.2x in the module and 15.7x in the generate block. The int reverse
chain increases about 14.4x. At this stage, the int module path does not use the
borrowed range optimization and remains superlinear in these samples; no
algorithmic improvement is claimed for it. Shared-machine load differed substantially: the 512-parameter
ranged module parsed in 425.629/78.977 ms before/after, and the ranged generated
input in 341.272/88.076 ms. These are sample timings, not universal speedup factors.
The operation-count tests independently establish the removed repeated work.

The after 512-parameter int module and generated inputs parsed in 51.683/56.513 ms,
with IR conversion 0.638/0.016 ms. Their ranged counterparts converted to IR in
0.585/0.016 ms. Grouped declarations, many signal/parameter alternations, four-state
prefixes, and many distinct scopes with large inherited tables still need separate
scaling measurements. This follow-up does not claim linear behavior for them.

## Looking up constant-folding types on demand

Starting from `5cc44d993`, ordinary `int` module parameters still spent quadratic
work in `parameter_value_env`. Each numeric value was already a sized literal,
but the two-state conversion enclosing it caused constant folding to scan every
visible binding and rebuild the parameter-type table. Declaration collectors
materialize these values several times, multiplying the repeated scans.

Mask-preserving constant folding now looks up a type only when substitution
encounters an identifier. Selection, concatenation and resize folding pass the
same lookup through their recursive calls. Literal-only two-state conversions
perform no type lookups. Existing map-taking typecheck APIs and the indexed-select
conversion entry point retain their signatures and supply equivalent map lookups.
No cache or scope lifetime is introduced. The width, signedness, mask and
expression-context evaluation rules remain unchanged (IEEE 1800-2023 11.8.1).

Regressions compare complete-table and direct-lookup results for every expression
shape, signed logical/arithmetic shifts, mixed signedness, unknown masks, unbased
fills, out-of-range selections, 129-bit values, function calls and unresolved
names. A 4,096-parameter unrelated environment does not add type lookups. The
production `parameter_value_env` path scans zero type-table entries for 16/64/256
numeric two-state parameters; a second counter bounds full AST construction's
remaining type-table scans in proportion to the parameter count. These internal
operation counts have no timing thresholds and add no shared executable cases.

The same optimized profile and three-fresh-run medians give:

| Workload | Parameters | AST before (ms) | AST after (ms) | Parse before/after (ms) |
| --- | ---: | ---: | ---: | ---: |
| int module declarations | 32 | 20.638 | 7.956 | 7.218 / 4.536 |
| int module declarations | 128 | 85.661 | 32.582 | 35.738 / 18.759 |
| int module declarations | 512 | 899.043 | 113.156 | 103.472 / 55.918 |
| int module declarations | 1,024 | 2,220.206 | 228.531 | 137.642 / 121.080 |
| int reverse generate dependencies | 32 | 12.409 | 8.532 | 5.811 / 4.392 |
| int reverse generate dependencies | 128 | 42.287 | 31.006 | 20.145 / 13.702 |
| int reverse generate dependencies | 512 | 138.360 | 121.087 | 58.867 / 54.530 |
| int reverse generate dependencies | 1,024 | 259.254 | 254.760 | 116.797 / 115.437 |

For the ordinary module, 32x input growth (32→1,024) increases after AST time
about 28.7x, compared with 107.6x before. The 1,024-parameter sample is about
9.7x faster. Parsing varied, especially at smaller sizes, so these are sample
measurements on a shared machine rather than universal speedup factors. IR
conversion before/after at 1,024 parameters was 2.308/1.164 ms for the module and
0.019/0.017 ms for the generate block. The reverse chain was already near linear
after the preceding prefix optimization and shows no material additional gain.

The ranged probe also passes at 32/128/512/1,024 parameters. Its after AST times
are 12.780/51.535/206.211/410.255 ms for module declarations and
14.091/52.504/209.856/428.733 ms for reverse generate dependencies. This is an
after-only range check for this step. All probes verify IR values/widths and
exclude backend compilation and simulation. The change removes the visible
environment factor from constant-folding type substitution; it does not establish
linear behavior for every expression shape or scope pattern.

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
modules with enums still rebuild some environments. Consecutive single-parameter
generate runs now reuse their prefixes; grouped declarations and interruptions
can still rebuild them. Numeric range headers borrow compatible environments,
while four-state or assignment-containing headers retain their copied contexts. Dimension/literal views now materialize once per
immutable scope in each collector. Many distinct scopes with large inherited
parameter/function tables can therefore differ from a single large block of
signals. Applying parameter dimensions and materializing scoped literals still
contribute work proportional to visible bindings in each distinct scope.
The flat-block probes do not establish linear scaling for those other workloads. The probes
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

The earlier type-query follow-up also validated the suite catalogue, including
retained-report integration tests, and its new shared case with both available
independent simulators. Current analyzer-only changes against unchanged cases do
not require external reruns. When executable shared cases or external adapters
change, use focused filters as described in `CONTRIBUTING.md`; routine validation
does not refresh checked-in verification reports or proof manifests:

```sh
cargo test --locked -p celox-test-suite --features verilator,icarus
cargo run --locked -p celox-test-suite --features verilator,icarus --bin verify-sv-verilator -- --filter generate::size_queries_use_generate_local_parameter_types
cargo run --locked -p celox-test-suite --features verilator,icarus --bin verify-sv-icarus -- --filter generate::size_queries_use_generate_local_parameter_types
```
