# SystemVerilog source layer

`celox-sv-syntax` owns original source files, UTF-8 byte spans, lossless tokens,
preprocessing, and native syntax arenas. It has no external parser or simulator
dependency. `celox-sv-analyzer` consumes these arenas directly for semantic
binding, elaboration and IR construction.

## Syntax and semantic ownership

`Declarations` retains source-item order, compilation-unit declarations and
nested design-unit ownership. Module, interface and package headers include
ports, parameter defaults, types, dimensions and original name origins. Flat
body-item arenas represent declarations, instances, continuous assignments,
procedures, subroutines, package bindings, modports and generate constructs.
Expression, type, statement and timing-control nodes reference arena IDs.

The analyzer binds native expression/type IDs and body-item membership throughout
module, interface, package and generate analysis. Shared `ParsedSource` arenas
support repeated parameter specialization without reparsing or copying the whole
source. Public semantic consumers use `ParsedSource`, `analyze_source` or
`analyze_packages`; `syntax::parse_source` returns native `Declarations`.

Grammar representation does not imply semantic support. Opaque body forms retain
explicit token ranges, and semantic consumers enforce their supported-language
contracts. Unsupported forms keep their existing diagnostics. There is no second
parser, reconstructed-source normalization or fallback to an original syntax
tree after native binding fails.

Native declarations cover ordinary variables/nets, parameter/localparam values
and types, typedefs and aggregate types, package imports/exports, DPI prototypes,
function/task declarations and instance argument/connection lists. Expressions
retain calls, casts, queries, assignment patterns, members and selections.
Procedures retain authored statement membership and lexical scopes independently
of execution-root selection. Statements cover blocks, conditionals, cases, loops,
foreach headers, assignments, calls, jumps, timing/wait controls and immediate
assertion actions. Concurrent assertions and other unsupported constructs retain
explicit grammar boundaries without adding execution behavior.

Generate traversal uses native scope IDs and explicit worklists. Existing case
comparison, genvar conversion, implicit localparams, scope numbering, imports and
declaration dependency ordering remain semantic concerns in the analyzer.
Interfaces use native modport/member ownership, connection expressions and
instance identities. Compilation-unit visibility uses native source-item order;
no synthetic package source is manufactured or reparsed.

Parsing, structural traversal, cloning and dropping use flat arenas and explicit
stacks. Tests exercise 10,000 nested expression, type, statement, timing and
generate constructs. Resource guards bound arena entries and pending worklists.
This syntax coverage does not imply equivalent depth through recursive semantic
IR processing.

## Preprocessing and source locations

Tokens retain physical spelling locations and macro invocation/definition chains.
Pasted and stringified tokens retain their contributing origins.
`SourceMap::logical_location` applies `line directives separately; physical spans
remain unchanged. `__FILE__` and `__LINE__` use logical locations. Ordinary and
macro-quoted strings retain UTF-8 spelling and numeric escapes; runtime byte
decoding belongs to the semantic layer.

Preprocessing supports object/function macros, empty actuals, defaults, nested
arguments, delayed expansion, aliases to function macros, escaped macro names,
continuations, token concatenation, ordinary macro stringification, `undef`,
`undefineall`, and nested conditional directives. Definitions persist across calls
on the same preprocessor. IEEE 1800-2023 coverage constants are predefined.
Compiler directives needed by syntax/semantic processing remain tokens.
Recursive macro expansion is diagnosed; expansion depth is bounded at 64.
Nested include/stringification contexts have a separate bound of 33.

The default include policy reads quoted/angle-bracket headers, expands filename
macros, searches relative/configured paths, preserves header origins and permits
32 nested includes. Conditional compilation must balance within each supplied
source/header. `Declarations::parse` and the analyzer compilation-unit API select
`IncludePolicy::Ignore`: header contents are not read. To read headers, preprocess
with `Read` and pass its source map and tokens to `Declarations::from_tokens`.
Malformed filenames and extra source after filenames are rejected in both modes.
Quoted include and `line filenames decode string escapes.

`Preprocessed::tokens` is the parser stream. Policy checks can use `observations`
or `inspection_text()`, which additionally retain active macro definitions and
undefinitions. Text rendering is an inspection/compatibility view, not a semantic
normalization step. Comments remain unless `strip_comments` is selected; removed
comments become separating whitespace.

Explicit preprocessing limits include SystemVerilog-2023 conditional macro
expressions, triple-quoted macro stringification and non-ASCII byte escapes in
filenames. They return source-located errors. Triple-quoted ordinary strings are
lexed. Native syntax and analyzer support remain a subset of the full language;
opaque grammar coverage does not claim full IEEE 1800 conformance.

The crate precedes the analyzer in publication order. Its first crates.io release
uses the [new-crate bootstrap procedure](../../.github/RELEASING.md#first-cratesio-release);
Trusted Publishing cannot create a new crate name.
