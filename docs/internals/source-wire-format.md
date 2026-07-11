# Private source-wire framing and staging

This document specifies the byte-level framing substrate and the first
untrusted source-node staging boundary for the verifier-first pipeline. It is a
construction specification, not an artifact format declaration. In
particular, it does **not** define `SourceWireV1`, make a source artifact
planner-ready, or permit an SLT arena to be frozen by itself.

The complete source schema still has to define the canonical typed HIR, every
expected source-value and source-control row, all provenance and gated-mux
registries, and their bidirectional input/output relations. Until that schema
and its aggregate verifier exist, this framing may be exercised only through
private decoder and adversarial-test entry points. There is no production
source-schema descriptor and therefore no production value that this decoder
can publish.

## Boundary and trust model

The intended ownership chain is:

```text
borrowed encoded bytes
  -> private raw envelope and flat raw tables
  -> verified typed source HIR and independently derived expected graphs
  -> private unclassified source-node stage and recomputed node facts
  -> complete source provenance and construction-identity verification
  -> prepared aggregate source artifact
  -> infallible commit to FrozenSourceArtifact
```

The framing and node-staging arrows covered here do not by themselves
establish the complete source relation. The raw decoder proves only that the
encoding is canonical, bounded by the supplied bytes, and representable on the
host. The unclassified node stage additionally proves append-order graph
structure and recomputes node facts against semantic input facts independently
derived from verified typed HIR. Neither result is a verified source artifact.

Raw integers remain raw through the complete structural scan. A raw node index
becomes `PhaseNodeId<SourcePhase>` only after every node edge, including edges
inside input indices, coercion uses, `ForFold` states/effects, concatenations,
and loop bounds, has been checked. Raw control, input, runtime-site, HIR, and
provenance integers similarly become their checked ID types only at the
aggregate verifier that owns their complete relation.

The following existing types are legacy/cache boundaries and are not inputs to
this decoder:

- `portable::StringTable` and `PortableVariable`;
- serde's `SLTNodeArenaWire` or standalone `SLTNodeArena::deserialize`;
- serde-deserialized checked control IDs;
- `SourceValueOccurrenceBoundary` by itself; and
- any legacy arena-only cache or optional provenance field.

No new verified record derives `Deserialize`. Compatibility is an explicit
version adapter into a legacy structural type, never relabeling into this
pipeline.

## Integer and scalar encoding

Every multibyte scalar uses little-endian byte order. Framing fields, IDs,
counts, widths, offsets, and lengths are unsigned. Wire data never contains
`usize`, a native pointer, an enum's Rust discriminant, or a Rust struct image.

| Meaning | Encoding |
| --- | --- |
| table-local and checked control/input/site IDs | `u32` |
| phase-node indices, counts, widths, offsets, and range lengths | `u64` |
| boolean | one byte, exactly `0` or `1` |
| closed enum | the schema-defined `u8`, `u16`, or `u32` tag |
| signed integer payload | fixed-width two's-complement field defined by its row schema |
| string or arbitrary-width integer bytes | a range into a canonical byte pool |

An enum tag not named by the exact aggregate schema is invalid. Reserved bytes
and flags must be zero. Conversion from `u64` to `usize` occurs only after the
value has been checked against both its table/pool bound and `usize::MAX`.
Conversion to a checked `u32` ID additionally requires that the complete dense
table length is representable by that ID namespace.

Strings are UTF-8 byte sequences. A schema's string table is strictly ordered
by raw UTF-8 bytes and contains no duplicate byte sequence. Other rows refer to
that table by checked `u32` string ID; they do not repeat or own string payloads.

An unsigned arbitrary-width integer uses minimal little-endian magnitude
bytes. Zero has length zero. A nonzero value's last byte is nonzero. Each
integer row owns one canonical byte-pool range, and every integer row is named
by exactly one scalar field in the decoded aggregate. This prevents several
large owned integers from being materialized from one small aliased byte span.
Semantic verification separately rejects payload or mask bits outside the
declared value width.

## Envelope header

The generic private envelope starts with this exact 40-byte header:

| Offset | Size | Field | Required value or interpretation |
| ---: | ---: | --- | --- |
| 0 | 8 | `magic` | ASCII `CELXRAW` followed by one zero byte |
| 8 | 2 | `framing_version` | little-endian `1` |
| 10 | 2 | `aggregate_kind` | exact kind requested by the internal schema descriptor |
| 12 | 4 | `schema_version` | exact version requested by that descriptor |
| 16 | 4 | `section_count` | number of 40-byte directory entries |
| 20 | 4 | `flags` | zero |
| 24 | 8 | `directory_bytes` | exactly `section_count * 40` |
| 32 | 8 | `payload_bytes` | exact number of bytes following the directory |

The header values are decoded field by field; the header is never transmuted to
a Rust type. The decoder receives one exact internal schema descriptor. It
does not accept a version range, guess an aggregate kind, or select the newest
known version. A kind or version mismatch is an ordinary structured error.

`aggregate_kind` assignments are reserved as follows for the eventual
aggregate schemas:

| Value | Aggregate |
| ---: | --- |
| 1 | source |
| 2 | occurrence |
| 3 | control value |

Reservation of kind `1` does not assign a source `schema_version`. A production
source descriptor must not be added until the complete `SourceWire` schema and
aggregate classification relation are specified. Private tests pass an
explicit test descriptor; its bytes are not a persistent artifact format.

The total input length must equal, using checked addition,
`40 + directory_bytes + payload_bytes`. Truncation and trailing bytes are both
noncanonical. Concatenated envelopes require an outer container and are not
accepted by this decoder.

## Section directory

The header is immediately followed by `section_count` entries of exactly 40
bytes each:

| Entry offset | Size | Field | Meaning |
| ---: | ---: | --- | --- |
| 0 | 4 | `tag` | schema-defined section tag |
| 4 | 2 | `kind` | `0` for fixed rows, `1` for a byte pool |
| 6 | 2 | `flags` | zero |
| 8 | 8 | `row_count` | number of rows, or bytes for a byte pool |
| 16 | 4 | `row_width` | exact encoded row width, or `1` for a byte pool |
| 20 | 4 | `reserved` | zero |
| 24 | 8 | `payload_offset` | offset relative to the start of envelope payload |
| 32 | 8 | `payload_len` | exact encoded byte length of the section |

The directory has one canonical representation:

1. Tags are strictly increasing. Unknown and duplicate tags are rejected.
2. The exact required/optional tag set and the exact `kind` and `row_width` of
   every tag come from the one selected schema descriptor. The descriptor also
   gives an optional section's canonical presence predicate; it may not permit
   both absent and present-empty encodings for the same value. An empty unknown
   section is still invalid.
3. `flags` and `reserved` are zero.
4. A fixed-row section satisfies
   `payload_len == row_count * row_width` using checked multiplication, and
   its schema-defined `row_width` is nonzero.
5. A byte pool has `kind == 1`, `row_width == 1`, and
   `payload_len == row_count`.
6. Sections partition the payload in directory order. The first offset is
   zero; every later offset equals the checked end of the preceding section;
   and the last checked end equals `payload_bytes`.
7. Padding, gaps, overlap, out-of-order physical sections, and bytes not owned
   by a section are invalid.

If the selected descriptor has no sections, `section_count`,
`directory_bytes`, and `payload_bytes` are all zero. A descriptor with required
sections cannot use that empty encoding.

ID, adjacency, operand, state, effect, range, and string-index pools are fixed
row sections with their schema-defined width, normally four or eight bytes.
Only uninterpreted UTF-8 and arbitrary-width magnitude storage uses byte-pool
sections.

This directory framing is generic. It does not define the complete list or
layout of source sections. Assigning those tags and row layouts is part of the
future complete `SourceWire` schema, not an incremental extension of a
partially accepted `SourceWireV1`.

## Flat table and pool canonicality

Recursive HIR and control shapes are encoded as flat rows with checked IDs.
Variable-length members are encoded as `{ start: u64, len: u64 }` ranges into
a section dedicated to that member role. A row never embeds a nested encoded
vector.

For each dedicated range pool, owner rows are visited in canonical table order
and their ranges must form an exact partition:

```text
first.start == 0
next.start == previous.start + previous.len
last.start + last.len == pool.row_count
```

With no owner rows, the corresponding pool must be empty. With owner rows and
an empty pool, every permitted empty range is at cursor zero.

Every addition is checked. This rule applies independently to node input
indices, concat operands, `ForFold` state rows, effect rows, effect arguments,
control predecessor/successor lists, occurrence operands, HIR children, and
all later source-provenance lists. A schema may require a range to be nonempty;
otherwise an empty range is canonical only at the current partition cursor.

Pool entries may repeat an ID when the language relation contains repeated
uses, but the repeated entry occupies repeated encoded bytes. Ranges may not
alias the same entries. Consequently one encoded entry cannot cause
input-dependent allocation amplification through many owner ranges.

String and arbitrary-width-integer byte ranges obey the same ordered exact-
partition rule in their respective row-table order. Other rows refer to the
resulting string/integer IDs rather than creating additional byte ranges.

Canonical source coordinates, table ordering, HIR child ordering, semantic
operand ordering, and provenance list ordering are semantic-schema rules. The
framing decoder validates scalar and range form; the selected aggregate
verifier validates those semantic orders independently from typed HIR.

## Two-pass decoder

Decoding is iterative and has two passes. It does not call a semantic verifier
during the first pass.

### Pass 1: validate and plan without owned allocation

The first pass:

1. reads the header and directory from borrowed bytes;
2. validates exact kind/version/tag/layout and the complete payload partition;
3. scans every fixed row for reserved bits, boolean/enum encodings, raw integer
   representability, checked table references, and canonical pool ranges;
4. validates UTF-8 and minimal arbitrary-width integer byte ranges in place;
5. computes with checked addition and multiplication the exact row counts,
   byte payloads, and owned staging sizes needed by pass 2; and
6. proves that every planned owned item has one corresponding encoded row or
   uniquely owned encoded byte range.

For each fixed table, the decoder proves at minimum that
`row_count <= payload_len / row_width` before converting the count or planning
storage. It separately checks `row_count * size_of::<RawRow>()` and the sum of
all planned staging allocations for `usize` overflow. There is no node, CFG,
depth, string, limb, or table-count policy limit: representability follows from
the actual encoded bytes and the host address space.

No `Vec::with_capacity`, `collect`, recursive descent, `BigUint` construction,
string copy, or `try_reserve` occurs in pass 1. An explicit worklist is used
where a schema-level flat relation still needs a traversal.

### Pass 2: fallible private materialization

Only after all pass-1 checks succeed does pass 2:

1. create empty private vectors;
2. call `try_reserve_exact` with the already checked counts for every vector;
3. decode fixed rows without changing a published object;
4. copy each aggregate-owned string/byte pool once; and
5. retain each validated arbitrary-width magnitude as one disjoint canonical
   range in that aggregate-owned raw byte pool until the joint typed-value
   verifier consumes it.

Failure of any reservation drops the private staging value. No partially
decoded table, checked ID, semantic fact, or artifact is returned. Production
code has no allocation retry, smaller fallback format, or semantic size
cutoff. A validated magnitude is decoded only into the aggregate's flat,
fallibly pre-reserved limb arena. Proof-path code never constructs
`num_bigint::BigUint`/`BigInt`, and a phase node names an independently
verified typed-value ID rather than owning payload/mask integers. Every limb,
scratch, row, and range allocation is therefore covered by the same structured
reservation-failure contract.

Tests use an injected `fail-at-N` reservation policy so every reserve site can
be failed deterministically. The production policy performs only the checked
`try_reserve_exact` calls; the test policy is not a runtime capacity setting.

## Unclassified source-node stage

The first node consumer is a private `UnclassifiedSourceNodeStage`. It is
constructed only after a typed-HIR verifier has independently produced the
exact semantic-object rows, exact input-access rows, and derived
`InputSemanticFacts<SourcePhase>` required by the raw node section. Object
identity and access identity are distinct: an input node names one exact
access, while a `ForFold` state target names the underlying semantic object.
Producer-supplied width, signedness, domain, dimension, access, or stride rows
are not semantic input facts.

Construction is ordered as follows:

1. Scan every child reference in every raw node without dereferencing it.
   Every child must exist and precede its owner. This scan covers all nested
   node-reference pools.
2. Check all raw input and runtime-site indices against the complete expected
   semantic tables, while they are still raw integers.
3. Convert one append-ordered node at a time to phase-typed IDs, checked ranges
   in pre-reserved flat payload pools, and a fixed
   `PhaseSLTNode<SourcePhase>` descriptor. Constant rows name the exact
   `VerifiedSourceTypedValueId` already derived by the joint aggregate;
   they never construct or own an integer payload here.
4. Recompute width, signedness, zero-mask, lowerability, access geometry, and
   structural coercion rules from the verified input facts and checked prefix.
5. Compare each node recipe and coercion context with the independently
   derived complete expected source-value graph.

The stage preserves the encoded node rows and does not intern them. In
particular, it does not:

- build the ordinary AVL index;
- reject raw equality between an ordinary mux and a gated mux;
- merge equal gated muxes belonging to different complete gated keys;
- infer ordinary identity from absence in an incomplete gated table;
- prove all-node expected-graph reachability; or
- create `FrozenSLTNodeArena<SourcePhase>`.

Those operations require the complete expected graph and verified source
provenance. Aggregate classification must derive exactly one
`OrdinarySemantic` or `Gated(complete source key)` identity for every node,
prove disjoint total coverage, and only then rebuild the ordinary index while
excluding gated nodes. The current ordinary-only `replay_typed` path is not a
wire decoder and must not be called on this unclassified stage.

The stage type has no public constructor, serializer, planner view, lowering
view, or `commit`/`freeze` method. Only the eventual aggregate source verifier
may consume it.

## Prepare, failure, and commit ownership

Byte decoding borrows `&[u8]`; every error therefore leaves the encoded input
unchanged. All decoded rows and node materialization live in private staging
owners which are dropped on failure.

Artifact identity uses one fallibly allocated, non-ZST live token, not a
serialized/global number or a durable address key:

```text
ArtifactBrandOwner
  private exactly-one BrandToken allocation created with try_reserve_exact
  not Clone/Copy/Serialize/Deserialize

BrandRef<'a>
  private &'a BrandToken

BrandedId<'a, P, K>
  BrandRef<'a> / compact private ID / phase and kind markers
```

`BrandRef` equality uses reference identity only while both owners are live.
The owner allocation is stable across aggregate moves, and Rust borrowing
prevents either token from being freed while a reference exists; allocator
address reuse therefore cannot make two live brands equal. No raw pointer,
`NonNull`, integer address, token address, global counter, random value, digest,
or split bit field is stored in a row/key or used as durable identity.

One `SourceConstructionSession` owns the brand, decoded input, staging pools,
and construction indices. A field-splitting editor borrows `BrandRef<'a>` and
may return `BrandedId<'a, ...>` handles. On every multi-handle operation it
checks brand equality before indexing, then stores only compact local IDs in
session rows. Neither the session nor any prepared/frozen part stores a
`BrandRef`, branded handle, or reference to its own owner. The borrow checker
therefore prevents `finish(self)` or moving/dropping the session while any
handle remains live; no self-referential aggregate is created.

`finish(self)` verifies all compact relations/capacity, drops transient
handles/indices, strips staging wrappers to unbranded prepared parts, and only
then moves the owner and parts together into `PreparedSourceAggregate`.
Failure returns the owned session/error so the top-level API can return every
unchanged input. Commit moves the same owner and unbranded parts into
`FrozenSourceArtifact` without allocation. Frozen APIs create new ephemeral
branded views borrowed from that owner; tables have no standalone constructor,
freeze, serializer, or public compact-ID constructor.

Brands are never proof-bearing wire fields. Deserialization rejects a legacy
brand field, verifies compact raw relations, creates a fresh owner, and reruns
the aggregate preparation. Decoding identical bytes twice therefore yields
distinct live brands; cross-artifact compact IDs compose only through an
explicit verified mapping relation, never equality by number or shape.

The complete future aggregate API has this ownership shape:

```text
try_prepare_source_aggregate(decoded complete raw source aggregate)
  -> Result<PreparedSourceAggregate,
            (decoded complete raw source aggregate, SourceAggregateError)>

PreparedSourceAggregate::commit(self) -> FrozenSourceArtifact
```

Preparation must verify the typed HIR, expected graphs, source-node recipes,
source provenance, roots/actions/observers/runtime sites, ForFold transition
semantics, gated registries, ordinary/gated classification, canonical indices,
and all-node reachability as one aggregate relation. It also reserves exact
cache-free final storage before returning success.

Commit only moves staged owned rows into already reserved storage and drops
construction state. It performs no allocation, semantic check, ID conversion,
map insertion, or fallible operation. A live producer-side builder follows the
same rule and is returned unchanged with its error when preparation fails.

No intermediate value in this document can be committed. This prevents an
arena, semantic-input table, or narrow source-occurrence topology from being
mistaken for a planner-ready source artifact.

## Structured errors

Decode and preparation failures use one closed, allocation-free error payload:

```text
SourceAggregateError
  rule: SourceAggregateRuleId
  phase: Header | Directory | RawRow | TypedHIR | NodeReplay |
         Provenance | Aggregate
  owner: None |
         ByteOffset(u64) |
         Section(tag) |
         Row(tag, u64) |
         PoolEntry(tag, u64) |
         RawNode(u64) |
         TypedOwner(typed owner kind, u64)
  context: None |
           ExpectedActual(u64, u64) |
           Range { start: u64, len: u64, bound: u64 } |
           Tag(u64) |
           Capacity { elements: u64, element_size: u64 }
```

The fields contain no `String`, `Vec`, copied source text, or allocator error
message. `Display` formats them lazily. Diagnostic source excerpts, if later
desired, are looked up under a separate bounded diagnostic policy and are not
part of the machine-readable failure.

The initially reserved stable rule IDs are:

| Rule ID | Meaning |
| --- | --- |
| `WIRE.HEADER_TRUNCATED` | the 40-byte header is incomplete |
| `WIRE.MAGIC` | the envelope magic differs |
| `WIRE.FRAMING_VERSION` | framing version is not exactly supported |
| `WIRE.AGGREGATE_KIND` | aggregate kind differs from the selected descriptor |
| `WIRE.SCHEMA_VERSION` | schema version differs from the selected descriptor |
| `WIRE.LENGTH_OVERFLOW` | checked encoded-length arithmetic failed |
| `WIRE.TOTAL_LENGTH` | truncation or trailing bytes violate exact length |
| `WIRE.DIRECTORY_SIZE` | directory byte length is not `count * 40` |
| `WIRE.SECTION_TAG` | a required tag is missing or an unknown tag is present |
| `WIRE.SECTION_ORDER` | section tags are not strictly increasing |
| `WIRE.SECTION_KIND` | section kind differs from the schema |
| `WIRE.SECTION_FLAGS_ZERO` | flags or reserved fields are nonzero |
| `WIRE.SECTION_ROW_WIDTH` | encoded row width differs from the schema |
| `WIRE.SECTION_LENGTH` | row count, width, and payload length disagree |
| `WIRE.SECTION_CONTIGUOUS` | sections do not exactly partition the payload |
| `WIRE.SCALAR_TAG` | a closed enum tag is unknown |
| `WIRE.SCALAR_BOOL` | a boolean is not zero or one |
| `WIRE.ID_REPRESENTABLE` | a raw integer cannot enter its eventual namespace |
| `WIRE.RANGE` | a checked pool range is outside its pool |
| `WIRE.POOL_CANONICAL` | pool ranges overlap, have a gap, alias, or lack coverage |
| `WIRE.UTF8` | a string payload is not UTF-8 |
| `WIRE.MAGNITUDE_CANONICAL` | an unsigned magnitude is not minimally encoded |
| `WIRE.STORAGE_AVAILABLE` | a planned private allocation failed |

Node and semantic verification retain their more specific stable IDs, such as
`GRAPH.CHILD_EXISTS`, `GRAPH.CHILD_PRECEDES_OWNER`, `INPUT.*`, `COERCION.*`,
and `FOR_FOLD.*`, with a `RawNode` owner until aggregate typing succeeds.
Errors never select a compatibility adapter, legacy allocator, retry, partial
artifact, or correctness fallback.

## Required adversarial fixtures

Before any production schema descriptor is added, private tests must cover:

- truncation at every byte offset of a valid nontrivial test envelope;
- trailing bytes and concatenated envelopes;
- wrong magic/kind/version, nonzero flags/reserved bytes, unknown/duplicate/
  out-of-order tags, and wrong row width;
- `u64::MAX` counts, count/width and offset/length overflow, and host-`usize`
  conversion failure;
- section gaps, overlaps, reversed physical order, and unowned payload bytes;
- pool range gaps, overlaps, aliases, noncanonical empty ranges, and incomplete
  coverage;
- invalid booleans/enums/UTF-8, duplicate or unsorted strings, leading-zero
  integer magnitudes, and width-exceeding value/mask bits;
- missing, self, and forward child IDs in every node-reference role;
- malformed accesses, input geometry, coercions, concat widths, canonical
  `ForFold` state rows, effect arguments, and arbitrary-width bounds/steps;
- equal raw muxes that later classify as ordinary/gated or as distinct gated
  owners, proving structural staging does not reject them prematurely;
- structurally valid but unexpected/unreachable nodes, proving only the later
  expected-graph relation rejects them;
- nonreciprocal/unsorted source adjacency and wrong occurrence operand arity,
  order, unit, or site when the complete provenance rows are added;
- deterministic failure at every `try_reserve_exact` site, with no published
  output and an unchanged live builder/input owner;
- non-ZST exactly-one brand-token allocation, moves of prepared/frozen owners,
  same-artifact composition, cross-artifact rejection before indexing, and
  allocation-address reuse stress after prior owners are dropped;
- compile-fail lifetime fixtures where a branded handle outlives, is stored in,
  or prevents finishing/moving its session, and source/occurrence phase handles
  cannot be interchanged;
- serialization contains no brand, decoding identical bytes twice creates
  distinct brands, no issuer/global-counter state exists, and commit performs
  zero allocator calls; and
- iterative 100k/1M-node graphs plus large concat and `ForFold` pools.

Tests for a narrow fixture schema demonstrate decoder safety only. They are not
evidence that a real source artifact verifies, that lowering has switched, or
that the pinned Heliodor performance gate passes.

## Explicit non-goals

This boundary deliberately does not:

- assign or publish `SourceWireV1`;
- define a partial accepted source language or silently omit unsupported HIR;
- expose an arena-only deserializer, freeze, planner, or lowering API;
- derive source input facts, coercions, truth semantics, or provenance from
  producer-supplied proof fields;
- classify a node as ordinary because it is absent from an incomplete gated
  registry;
- use serde or a legacy portable/cache type as current-wire verification;
- add a node, CFG, depth, payload, or iteration cutoff;
- retry allocation or fall back to a legacy representation;
- change symbolic evaluation, SIR/MIR lowering, regalloc, or backend output; or
- claim end-to-end performance from structural decoder measurements.

The first production source descriptor is permitted only after every source
section and closed variant, all independently derived expected rows, the
ordinary/gated total classification, and the full aggregate prepare/commit
relation have been specified and implemented together.
