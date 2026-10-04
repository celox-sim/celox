# 共有ポートの合成仕様と正例・反例（0.8 / JSON version 3）

0.9ではこのversion 3にも入力量化・executionモード・ensure・短いtrace呼出しを追加した。
新しい意味と制限は [expectations and traces](expectations.md) を参照。
この文書のpositive/negative例は従来の存在条件を保持する。

`.hwv` の `specification "名前"` ヘッダは、関係として書いた仕様部品、その合成、
許したい／拒否したい有限の振る舞いを一つの文書にまとめる。
ヘッダにセミコロンを付けず、以降の宣言を囲む外側の波括弧も不要。
[budgeted_counter.hwv](../examples/budgeted_counter.hwv) は「カウンタの算術」と
「使える予算」を別部品にし、入出力を共有して組み合わせる例。
既存の `design` / version-2 JSON はそのまま使用できる。
このページは共有グローバルポート・同名同期操作のversion 3を説明する。
局所ポート、明示配線、インスタンスごとの状態分離、独立した操作集合は
新しいversion 4の [scoped specifications](scoped-specifications.md) を参照。

```
cargo run -- examples/budgeted_counter.hwv --check
cargo run -- examples/budgeted_counter.hwv --out /tmp/budgeted --z3 /path/to/z3
cargo run -- examples/contradictory_composition.hwv --out /tmp/contradiction --z3 /path/to/z3
```

最後の例は意図的に失敗する。部品単体で許される振る舞いが、合成後も許されるとは限らない。
`weakened_budget.hwv` も意図的な失敗例で、弱くしすぎた仕様が負例を許すことを検出する。

## 部品の意味

仕様全体で `input`、`observation`、名前付き `operation` を宣言する。
各 `component` は独立した `state` 宣言、初期条件 `init`、各状態での `invariant`、
操作ごとの関係 `steps` を持つ。

```
specification "Counter"

input amount: bv<4>;
observation count: bv<4>;
operation add {}

component Counter {
  state value: bv<4>;
  init s.value == 0u4;
  invariant o.count == s.value;
  steps { add = n.value == s.value + i.amount; }
  // example declarations can follow here
}
```

`o.count` と `i.amount` は、文書の observation / input 宣言を参照する。
幅・算術・演算子の意味は [language guide](language.md) と同じ。

- `s.x`: その部品の現在の非公開状態
- `n.x`: その部品の次の非公開状態
- `o.x`: 現在の共有観測
- `no.x`: 次の共有観測
- `i.x`: 今回の操作が消費する共有入力

init / invariant は s と o だけを参照する。steps は全ての名前付き操作を明示的に定義する。
`true` はその操作を無制約にする。状態を保持したいときは `n.x == s.x` のように明示する。
複数部品が同じ private field 名を使っても、別の状態として扱う。

## 合成

```
composition Arithmetic { members compose(Counter); }
composition Budgeted { members compose(Arithmetic, Budget); }
```

名前付き composition をさらに合成できる。選んだ部品の関係を、同じ観測・同じ入力を
使って論理積にする。ある例が単体で通るときの witness と、別の部品が通るときの witness
を別々に採用して成功にはしない。共有値について一つの整合した実行が必要になる。

各 trace の操作名に対し、全メンバーが同時にその操作の関係を満たす。暗黙の非同期実行、
interleaving、任意の順序変更は行わない。別々の private state を持ち、観測と入力だけを共有する。
ポート名の変更・配線別名・パラメータ付きインスタンス化はこの版にはない。

未知のメンバー、循環、空の合成、同一 component の重複到達はエラー。
例えば P が A を含むとき `compose(P, A)` は拒否する。独立したコピーが必要なら別名の
component を定義する。合成の循環検査は、将来の証明仮定の循環検査とは別の機能である。

## 正例と反例

各 component / composition の中に、名前付き `example` を直接並べる。

```
example add_two {
  expect positive;
  initial { count = 0u4; }
  trace {
    add { inputs { amount = 2u4; } observe { count = 2u4; } }
  }
}
example wrong_result {
  expect negative;
  initial { count = 0u4; }
  trace {
    add { inputs { amount = 2u4; } observe { count = 3u4; } }
  }
}
```

`operation add {}` は文書ルートでの操作の宣言。trace中は従来通り
`add { ... }` と書き、操作を一回行うことを表す。

- positive: 指定した観測と入力に一致する、仕様上の実行が**一つ以上存在する**ことを要求する
- negative: 非公開状態や省略した値をどう補っても、その実行が**存在しない**ことを要求する

initial は frame 0 の観測。init と invariant を frame 0 で課す。
trace の step k は inputs k を使い、frame k と k+1 を関係づける。
`observe` は操作後の frame k+1 を制約し、全ての frame に invariant を課す。
reset 操作は暗黙には挿入しない。初期述語が例の出発点を決める。

入力・観測は部分指定できる。省略は0ではなく存在量化される。
trace の inputs / observe ブロックを省くと空の部分指定になる。
非公開状態の値を例から直接指定することはできない。
同じ操作を何回書いても順番通りの別ステップとして保持する。空の trace も許す。
一例の上限は4096ステップだが、これは時間・メモリの厳密な上限ではない。

各例について単一の SAT 問題を作る。SAT の場合は具体的な全状態・観測・入力を取得し、
再度 SAT を確認する。UNSAT は全ての隠れた補完を排除したことを意味する。
UNKNOWN・solver異常・witness再取得の失敗を、例の成功として扱わない。

正例だけでは仕様が弱すぎることを見落とし、反例だけでは全部を拒否する矛盾した仕様を
見落とす。両方を付け、特に合成後にも要求する振る舞いを置く。
例が通ったことは、任意長の実行、仕様の完全性、deadlock freedom、liveness の証明ではない。
指定した有限 prefix が存在しても、それを任意に延長できるとは限らない。
report の coverage には部品・合成ごとの正例／反例数と不足する極性の警告を記録する。
負例しかない対象では、それらが通っても許される振る舞いの存在を示したことにはならない。

## 実装への限定された binding

任意の composition に、同じ文書の `implementation` を結び付けられる。
実装は state/reset/next と、操作を選ぶ Boolean 式を持つ。
全 private state と共有観測に、実装の現在状態だけを使った写像を与える。

```
implementation {
  composition Budgeted;
  reset_input rst;
  state count: bv<4>;
  state remaining: bv<4>;
  reset { count = 0u4; remaining = 3u4; }
  wires { accept = i.go && i.amount <= s.remaining; }
  next {
    count = if w.accept { s.count + i.amount } else { s.count };
    remaining = if w.accept { s.remaining - i.amount } else { s.remaining };
  }
  operations { add = w.accept; }
  binding {
    bind Counter { value = s.count; }
    bind Budget { credits = s.remaining; }
    observation count = s.count;
    observation remaining = s.remaining;
  }
}
```

共有入力に `input rst: bool;` と `input go: bool;` を宣言する。
`bind Counter { ... }` は既存componentの状態への写像であり、新しい状態宣言ではない。
binding内の `observation count = ...;` は共有観測への写像で、
ルートの `observation count: bv<4>;` はその型の宣言。
reset は最優先で、抽象初期状態に戻る。
全 binding field と操作 selector の過不足を検査し、入力依存の abstraction は拒否する。
証明する内容は次の通り。

1. reset が合成仕様の init / invariant を確立する
2. reset 以外で、選択される操作が高々一つである
3. 選択された操作について、写像後の一歩が全 component の関係と次状態 invariant を満たす
4. 操作が選択されない一歩では、写像した private state と観測が全て保持される

これは与えた状態写像による safety / stuttering refinement。
到達可能性を絞り込まず、写像後の invariant を満たす全実装状態で検査する。
関係的仕様の全ての振る舞いを実装できることや、abstraction の全射性は要求しない。
全 selector が常にfalseで、状態を保持する実装は safety を満たし得る。
進行・公平性・停止性は別途必要であり、specの正例が通ることも実装の進行保証にはならない。

## 結果と証跡

主な結果は `spec_examples_passed`、`spec_examples_failed`、`unknown`、`no_examples`。
binding がある場合は `spec_examples_and_binding_verified`、`implementation_binding_failed`、
`binding_verified_no_examples` も使う。validation-only は従来通り `validated`。
report では例の成否と `implementation_binding` の証明結果を分離して確認できる。

元の `.smt2`、solver の `.out`、完全な witness の context mapping を保存する。
Rust の検査・変換・カーネルと Z3 を信頼する。今回の変更で処理系全体の健全性を
形式的に証明したわけではない。
証明の仮置き・穴・条件付き定理・証明依存DAGはまだ実装していない。

## 0.8の表記と互換性

旧 `specification "名前" { ... }`、`operations { add {} }`、
`components { Counter { ... } }`、`compositions { Budgeted { ... } }`、
部品・合成内の `examples { overspend { ... } }` は互換表記として受理する。
型宣言の旧 `inputs { ... }` / `observations { ... }` / `state { ... }` と、
bindingの旧 `states { Counter { ... } }` / `observations { count = ...; }` も使える。
個別宣言と旧コンテナは同じJSON mapへloweringし、形式をまたぐ重複名も拒否する。

`input` / `observation` / `operation` / `component` / `composition` / `example` /
`state` / `bind` 等は文脈で解釈する。新しいグローバル予約語は増やさない。
`steps { add = ...; }`、実装の `operations { add = ...; }`、
traceの `inputs { amount = ...; }` / `observe { count = ...; }`、
`reset` / `next` / `wires` 等の代入ブロックは変更しない。

DSLでは宣言がない集合の空コンテナを書かなくてよい。
ルートの入力・観測・操作・部品・合成、component/implementationの状態、
部品・合成の例、bindingの状態写像・観測写像を省略すると、
対応するJSON mapに `{}` を補う。JSON入力では必須キーと空objectを明示する。
これは空集合の表記だけの変更で、意味検査を省略するものではない。
例えば操作・部品は引き続き一つ以上必要で、合成の `members` は空にできない。
`init` / `invariant` / `steps`、例の `expect` / `initial` / `trace` 等の
必須フィールド・ブロックも従来通り必要。

```sh
python scripts/json_to_hwv.py examples/budgeted_counter.json /tmp/budgeted_counter.hwv
cargo run --locked -- /tmp/budgeted_counter.hwv --emit-json /tmp/budgeted_counter.json
```

printerはversion 2 / 3 / 4 JSONを対応する表記へ変換し、式木・trace順序を保持する。
このページの文書はversion 3のままで、version 4へは自動変換しない。
移行後もRust側の完全な検査を通す。JSON schema versionと処理系の版番号は別。

## version-3 JSON の構造

完全な入力例は [budgeted_counter.json](../examples/budgeted_counter.json)。
型と式の配列表現は version 2 と共通で、例えば `0u4` は `["bv",4,0]`、
`s.x == o.count` は `["eq","s.x","o.count"]`。

トップレベルの必須フィールド:

- `version`: 3
- `kind`: `"specification"`
- `inputs`, `observations`: 名前→型の object
- `operations`: 操作名→空 object の object（例: `{"add":{}}`）
- `components`: 名前→component の object
- `compositions`: 名前→composition の object（空でもよい）

任意フィールドは `name` と `implementation`。
component の必須フィールドは `state`、`init`、`invariant`、`steps`、`examples`。
steps は全操作名→Boolean式、examples は例名→example の object。
composition は `members`（component/composition名の配列）と `examples` を持つ。

example は `expect`、`initial`（観測名→式）、`trace`（ステップの順序付き配列）を持つ。
旧 `expect` は `"positive"` / `"negative"`、0.9の明示モードは `"exists"` / `"not_exists"` / `"forall"`。
明示モードでは任意の順序付き `quantifiers` 配列を指定でき、JSON内ではその変数を `q.name` で参照する。
通常の `.hwv` ソースでは宣言名をそのまま `name` と書く。旧 `q.name` も互換表記として受理する。
旧モードには `quantifiers` を付けない。各ステップは必ず
`operation`（操作名）、`inputs`、`observe` を持ち、任意のBoolean `ensure` を追加できる。
inputs / observe は名前→式のobject。束縛変数がない場合は従来通り閉じた式。
ensureは操作後の `o.` 観測値と束縛変数だけを参照する。
JSON側では、部分指定がなくても明示的に `{}` とする。DSLではtrace stepの
inputs / observeを省いた場合にフロントエンドが `{}` を補う。
exampleの `initial` と `trace` は空でもブロックを明示する。

implementation の必須フィールドは `composition`、`reset_input`、`state`、`reset`、
`next`、`operations`、`binding`。`wires` は任意。
operations は操作名→Boolean selector式、binding は `states` と `observations` を持つ。
states は component名→（private field名→状態のみの式）、observations は
観測名→状態のみの式。全ての対応先と型を検査する。

## Conditional bounded response

Safety/stuttering alone permits an implementation to select no operation forever.
An optional `implementation.responses` contract adds an inductive bounded-response
check. The same contract is available in v3 and v4. Complete runnable examples are
[response.hwv](../examples/response.hwv) and
[scoped_response.hwv](../examples/scoped_response.hwv).

```hwv
responses {
  request_done {
    operation advance;
    accept w.accept;
    pending s.busy;
    rank s.ticks;
    bound 2;
    cover_depth 2;
    assume !i.stall;
  }
}
```

`accept` is the implementation's explicit acceptance handshake, not merely an
external request signal. Completion is the selected operation's existing Boolean
selector on the current implementation edge; its effects must still satisfy the
ordinary relational safety binding. Acceptance may complete on the same edge.
Otherwise `pending` must become true in the next state, and completion must occur
within `bound` subsequent nonreset edges. The example accepts at edge 0, waits at
edge 1 and completes at edge 2, incrementing its abstract counter exactly once.

The initial interface supports exactly **one contract per implementation and one
outstanding request**. Accepting while pending fails a checked obligation, even
if the same edge completes the old request: same-edge queue replacement is not
supported. Requests presented while busy may remain unaccepted; eventual
acceptance is not promised. Unsolicited completion is rejected. `pending` cannot
be silently dropped or cleared before completion. Reset has priority, cancels
the obligation and requires the reset state to have `pending == false`; acceptance
and completion on a reset edge do not create or discharge a nonreset obligation.

`pending` and `rank` are state-only expressions over `s.*`. `rank` must be an
unsigned word of at most 64 bits; `bound` must be a positive integer representable
in that width. No wraparound, saturation, ghost state, implicit queue or temporal
syntax is added. The author supplies the real implementation-state witness.
Unsupported fields, input-dependent ranks/pending predicates, multiple contracts,
unknown completion operations and invalid bounds are rejected during validation.

`assume` is a Boolean expression over **inputs only**, using `i.*`; implementation
state and wires are excluded so it cannot hide a stuck implementation state.
It must hold at acceptance and every subsequent nonreset edge until completion.
The example excludes stalls explicitly. Changing its assumption to `true` fails:
an indefinitely stalled implementation does not meet a physical-step deadline.
There is no implicit fairness or removal of stalled ticks from the count. After
an assumption violation the old request has no response guarantee. Independently
of response assumptions, the original safety obligations remain unconditional.

For pending predicate P, acceptance A, completion C, rank R and bound B, generated
checks establish reset cancellation, no overlapping A/P, and, on an assumed
nonreset step, `P_next == (P || A) && !C`. They establish `P_next => R_next < B` and
`P && !C => R_next < R`. The latter uses the same unsigned-rank comparison as v2
progress. The invariant `P => R < B` is established at acceptance and preserved;
strict decrease prohibits a non-completing step at rank zero. Thus the bound
holds for each accepted request by induction, not by enumerating a finite trace
and extrapolating it into unbounded liveness. Other operations receive no progress
claim, and request/response payload correspondence is limited to the separate
relational safety specification.

**A verified conditional response does not establish that an external request is
accepted, or that the implementation ever does useful work after reset.** `accept`
is an implementation-defined event, not an automatically enforced request
handshake. There is no separate external request predicate or request-to-acceptance
obligation in this version. A model can add `ever_enabled`, reset it to false,
preserve it forever, and require it in `accept`. Acceptance can then be feasible
only in unreachable states, while every reset-reachable run remains idle and the
conditional response checks pass. The regression suite preserves this example.

Optional `cover_depth N` (1–32) requests a **separate bounded acceptance cover**.
Edge 0 applies the original reset assignment with reset high. Edges 1 through N
are candidate acceptance edges, each with reset low and the declared input
assumption satisfied. The reset input vector is independent of subsequent input
vectors, and the assumption is not imposed on reset. There are no constraints on
inputs after the accepted edge. Depth counts nonreset implementation edges after
reset, independently of the response latency `bound`.

`adequacy.reset_acceptance_cover` reports the depth, limits, source location and:

- `reached`: a SAT prefix independently replayed through the original reset and
  next-state expressions. `witness.trace` contains concrete inputs and before/after
  states, ending at `witness.acceptance_edge`; `original_transitions_validated`
  is true. This proves existence of one accepting run, not service for all requests.
- `not_reached_within_bound`: UNSAT for this bounded cover only. It does **not** mean
  acceptance is unreachable at greater depth. The `ever_enabled` mutant receives
  this result while its conditional response theorem still verifies.
- `unknown`: budget exhaustion, unsupported scalar replay, or failed witness
  validation; no replay-validated witness is returned.
- `unchecked`: no `cover_depth` was supplied.

The cover uses the existing finite solver and word-level interpreter, not an
external solver or finite traces extrapolated into liveness. Initial replay
support is Bool/BV state and inputs of width at most 64, with at most 4096 frame
symbols; reported finite solver budgets also apply. It adds no mapped-invariant,
pending or rank assumptions to the original transition system. Its status never
changes the conditional proof status or CLI verification exit status.

`adequacy.reset_reachable_acceptance` mirrors the cover status;
`adequacy.external_request_to_acceptance` remains `"not_specified"`. The LSP shows
cover outcomes at `cover_depth` and retains a warning at `accept` about the missing
external request-to-acceptance obligation, including when the cover is reached.

Separate SAT checks require a feasible nonreset input assumption and a feasible
acceptance under the mapped invariant. Contradictory assumptions or `accept false`
produce `failed_nonvacuity`, not success. These witnesses are **not claims of reset
reachability**, general specification adequacy or deadlock freedom. Unknown in any
required obligation remains Unknown. A verified response requires all generated
obligations and the existing implementation binding to pass.

```sh
cargo build --release --locked
HWVERIFY_SOLVER=finite target/release/hwverify-rs examples/scoped_response.hwv --out /tmp/fresh-response
cargo test --release --locked -p hwverify-rs --test responses
```

Read `implementation_binding.responses` and the `response_*` obligations in the
report. Native reports attach `source_location` to each response obligation;
finite counterexample files retain current/next implementation and response
aliases. In the editor, **Check implementation responses and safety** explicitly
runs the same checker; edit-time analysis only validates syntax, names and types.
The editor action also checks the document's ordinary examples and safety binding,
so a passing response row alone does not override other failures in that result.

Failed or Unknown implementation bindings also produce a blocking implementation-level
editor diagnostic naming the outstanding obligations, even when every response
obligation passes.

## Reset-reachable failures and source RTL replay

`hwverify-replay` adds bounded **failure** search and strict concrete replay for
native v3 relational implementations. It is separate from both inductive checking
and the positive acceptance cover above. Initial scope is scalar Bool/BV≤64,
one synchronous positive-edge clock, canonical reset at edge 0 followed by
1–32 nonreset edges, and direct top-level state/input bindings. Native v4, arrays,
multiple clocks, asynchronous/typed reset ports, and four-state replay are not
supported by this first source-replay route; existing checkers are unchanged.

Build with `cargo build --release --locked -p hwverify-rs`. The executable accepts
one JSON request on stdin; use `source` for native v3 text or `document` for its
canonical JSON. For example, a request has `version: 1`, `mode: "search"`,
`goal: "safety"`, `depth: 4`, and `source: "..."`. Goals are:

- `safety`: the original reset, operation-exclusion, relational-step, invariant,
  and stutter failure predicates shared with the inductive v3 binding checker.
  Response input assumptions do not restrict this safety search.
- `response_deadline`: a separate monitor for the sole declared response. It
  starts on qualifying acceptance, permits same-edge completion, and fails if
  completion is absent through the Bth subsequent nonreset edge. It maintains
  its own outstanding-request bit and age, never trusting DUT `pending` or rank.
  An assumption violation cancels the affected interval's guarantee, not later
  qualifying requests. The monitor does not add request-to-acceptance, payload,
  overlap, or unsolicited-completion guarantees; those are separate properties.

Search statuses are `reset_reachable_failure`, `bounded_no_failure`, and `unknown`.
Only the first carries an original-transition-and-property-validated witness.
`bounded_no_failure` means no violation of the selected goal within the bound,
not an unbounded proof. SAT states are compared against a fresh execution of the
original reset/next expressions; supplied intermediate states are never inputs.
The witness includes its complete canonical model and goal. `mode: "replay"`
rechecks a supplied `witness` and rejects stale models, corrupt frames, wrong reset
indexing, positive-cover records, and induction countermodels. `check_stimulus`
executes explicit `inputs` against a selected goal and reports a concrete
`trace_no_failure` or actual failure; it makes no universal claim.

The [Celox replay gate](../conformance/celox-replay/run_ci.sh) performs the full
source workflow. It compiles handwritten Veryl with the existing pinned frontend,
lifts DUT reset/next expressions without importing expected values, searches for
an actual failure, and drives only the resulting external inputs into an actual
Celox native simulator. Inputs settle first; acceptance/completion are sampled
before exactly one clock event; sequential state is sampled afterward. Celox's
post-edge combinational settle is never substituted for the pre-edge sample.
Reset must establish all selected state independently of power-on values.

The simulator subprocess receives **no expected outputs or property oracle**.
The independently authored native specification supplies the property, and the
original transition replay supplies the trace to compare. A simulator/model
mismatch is `simulator_divergence`, not a reproduced property failure. Celox and
the lifter share frontend dependencies, so agreement is not an independent proof
of frontend correctness. The source/property/binding/model hashes, pinned Celox
revision and dependency patch identity are checked before saved stimuli run.
Regressions retain stimuli and identities rather than simulator-generated goldens.

The gate uses Celox `124a1315096d21b85d9d0d84fd7139363a181cad` (0.8.2), Veryl
0.21.0, and the existing named frontend patches. No upstream files or original
suite cases are changed. The adapter has a separate locked workspace retaining
upstream package versions. Its host-runtime dependencies must be available; a
missing simulator or build/runtime error fails CI rather than skipping replay.
Run `./conformance/celox-replay/run_ci.sh /tmp/fresh-celox-replay` with Rust 1.98.1.

Fixtures exercise an enabled counter's wrong update and a single-outstanding
request's dropped/deadline failures. Every saved failure is replayed on both the
mutant and correct source under identical stimuli. An unreachable-only fault
retains its induction countermodel but has no bounded reset failure and passes
concrete reset traces. Failure controls also reject internal-state writes,
malformed/stale witnesses, bad sampling/reset indices, and simulation divergence.
The mandatory `celox-replay` CI job uploads fresh evidence; `--record` is an
explicit developer action for changing reviewed regressions and is never used
by CI.

### User project entry point

The fixture gate and external projects use the same public CLI,
[project.py](../conformance/celox-replay/project.py). No fixture name, source layout,
or physical port naming convention is required. First build the pinned tools
from the hwverify repository (Rust 1.98.1):

```sh
python3 conformance/veryl-proof/prepare.py
cargo build --locked --manifest-path conformance/veryl-proof/frontend/Cargo.toml --target-dir conformance/veryl-proof/target
cargo build --locked --manifest-path conformance/celox-replay/adapter/Cargo.toml --target-dir conformance/veryl-proof/target
cargo build --release --locked -p hwverify-rs -p hwverify-sir
```

Place a manifest in your project directory. This example maps the canonical
names in a native counter specification to differently named RTL pins:

```json
{
  "version": 1,
  "name": "my_counter",
  "sources": ["rtl/counter.veryl"],
  "top": "MyCounter",
  "specification": "contracts/counter.hwv",
  "clock": "clock_pin",
  "reset": {"input": "rst", "active": 0},
  "inputs": {"rst": "reset_pin", "en": "enable_pin"},
  "state": {"count": "count_pin", "fault": "fault_pin"},
  "signals": {},
  "property": "safety",
  "depth": 8
}
```

All fields are required. `sources` may list multiple Veryl files; file paths are
relative to the manifest directory and must remain inside it. `top` selects the
module. `inputs` maps every specification input to exactly one external nonclock
RTL input; the boolean `reset.input` must equal the specification's `reset_input`.
`reset.active` is integer 0 or 1. `state` maps every implementation state variable
to a distinct top-level sequential signal. Names on each side may differ.
Types come from the native implementation declarations and must match the RTL
widths exactly. The counter example needs a native declaration for both `count`
and `fault`; adjust the mappings and declarations together for your design.

The native v3 file contains the relational components, implementation state
schema, abstraction binding, operation selectors, and optional response contract.
Its implementation `reset`, `next`, and wire expressions are typechecked template
fields, then **replaced by the expressions lifted from your source RTL**. They
are not taken as an independently implemented DUT or used to supply expected
outputs. The relational requirements and state/observation bindings remain the
user-authored property.

`signals` maps each declared implementation wire alias to a typed pre-edge DUT
signal. For example, aliases used by a response contract can be mapped as
`"accept": {"signal": "accepted_pin", "type": "bool"}` and
`"complete": {"signal": "done_pin", "type": "bool"}`. Use `"property":
"response_deadline"` to check the single declared response. Signal types are
`"bool"` or `{"bv": N}` for N=1–64; they must match the RTL. All declared wire
aliases must be mapped, with no extra aliases. These signals are also sampled
before the simulator clock edge and compared against the deterministic lifted
DUT expressions. No unique output is inferred from a relational-specification
witness: a relation allowing several next values continues to allow all of them.

Search and save a reproduced failure, then replay it without solver-generated
stimulus selection:

```sh
python3 conformance/celox-replay/project.py search /path/to/project/replay.json --out /tmp/my-search --save-regression /path/to/project/failure.json
python3 conformance/celox-replay/project.py replay /path/to/project/replay.json /path/to/project/failure.json --out /tmp/my-replay
```

Output directories must be fresh. `--save-regression` is optional and never
replaces an existing file. It writes only after an actual failure passes both
original-property replay and Celox simulation. A bounded no-failure or Unknown
result does not create a regression. Saved version-2 regressions contain concrete
inputs and hashes of the manifest, ordered sources, native specification,
resolved bindings, compiled transition model and dependency identity. Moving an
unchanged project directory is allowed; changing its relative layout, content,
mappings, property or depth requires a new search instead of silently accepting
an old regression.

The CLI prints a JSON result and retains details in the output directory.
Exit 0 means the request completed: inspect `status`, which can still be
`reset_reachable_failure`, `bounded_no_failure`, or `unknown`. Malformed projects,
stale regressions and tool failures return `project_error` with exit 2;
`simulator_divergence` remains a separate result with exit 2. No simulator match
is reported for a search with no executable failure witness.

Unsupported mappings fail explicitly: missing or duplicate input/state mappings,
hierarchical names, array lanes, width mismatches, state mapped to combinational
outputs, clock-as-data, inouts, typed/asynchronous reset, arbitrary mapping
expressions, unknown fields, duplicate JSON fields, and project paths escaping
the manifest directory. The existing lifter additionally rejects unsupported
SIR and reset equations depending on arbitrary prestate. This interface retains
the scalar two-state, one-clock, synchronous-reset scope described above.

## AXI4-Lite library

[protocols/axi4lite.py](../protocols/axi4lite.py) generates reusable native v3
components and state-only bindings for sampled AXI4-Lite safety. The normative
reference is [Arm IHI 0022H, ID040120](https://developer.arm.com/-/media/Arm%20Developer%20Community/PDF/IHI0022H_amba_axi_protocol_spec.pdf),
not a claim to support every provision of that document. `rules()` exposes the
same rule identifiers, owners and sections in machine-readable form.

| Supported check / obligation ID | Reference | Independent regression |
| --- | --- | --- |
| `{aw,w,b,ar,r}_valid_stable`, `{aw,w,b,ar,r}_payload_stable` after a stalled sample, including the accepting edge | A3.2.1–A3.2.2 | Every channel and payload field; dropped VALID and changed payload |
| `b_requires_aw_w`: previously accepted address **and** data, separately counted; consume both on B handshake | A3.3–A3.3.1 | AW-first, W-first, simultaneous; missing half, same-edge premature response, duplicate B |
| `r_requires_ar`: previously accepted AR; consume one on R handshake | A3.3–A3.3.1 | Backpressure and continuous transfers; unsolicited, same-edge premature and duplicate R |
| `b_response_code`, `r_response_code`: exclude EXOKAY | B1.1.1 | Reject 1; permit 0, 2, 3 |
| `{manager,subordinate}_reset_valid`: reset-established VALID values are low | A3.1.2 | Each VALID; real reset-dependent combinational-output mutant |

AW and W have independent acceptance counts. Counts represent accepted requests
minus accepted responses; current-edge acceptance cannot justify current BVALID
or RVALID. A full queue may accept and retire simultaneously. Outstanding work
may remain indefinitely: **there is no READY fairness assumption or protocol
response deadline**. These checks do not prove eventual response or lost-response
freedom. Optional performance requirements must be separate explicit bounded
response contracts with their own environment assumptions.

Parameters are `address_width` 1–64, `data_width` 32 or 64, `capacity` 1–16 per
AW/W/AR count, and `role` (`manager`, `subordinate`, or trace-only `link`). Capacity
is a verification bound, not an AXI restriction. Exceeding it is reported as
`scope_exceeded`, never silently wrapped or declared a protocol violation.
Both data widths, all strobe bits (including zero), protection bits and response
bits participate in stability checks. This does not validate their functional
meaning for a particular register map.

For a manager DUT, AW/W/AR source rules are guarantees and B/R source rules are
counterpart assumptions; a subordinate swaps those roles. The generated observer
records counterpart violations separately and stops interpreting subsequent
samples in that reset epoch as a legal environment. A DUT violation found earlier
is retained. No DUT-owned rule is assumed. `link` checks both ends without any
counterpart assumptions. Composition does not discharge an environment assumption
by referring back to the same DUT guarantee. To close an interface, check the
complete sampled link or independently establish the counterpart contract.

### AXI source binding and replay

After the builds described in [User project entry point](#user-project-entry-point),
run the checked examples:

```sh
python3 protocols/axi4lite_project.py search examples/axi4lite/binding.json --out /tmp/axi-subordinate
python3 protocols/axi4lite_project.py search examples/axi4lite/manager-binding.json --out /tmp/axi-manager
./conformance/axi4lite/run_ci.sh /tmp/axi-ci
```

The [subordinate](../examples/axi4lite/subordinate.veryl) has independent AW/W slots
and one read slot; the [manager](../examples/axi4lite/manager.veryl) issues one
transaction of each kind and holds requests under backpressure. They illustrate
protocol control, not a functional peripheral or a certified AXI implementation.

Copy the example project files into your own directory, replace the RTL, and
provide a native v3 specification and [project manifest](../examples/axi4lite/project.json).
The first version requires one unconditional `tick` operation. Existing
components and bindings remain in the checked composition; event-selective and
v4 bindings are rejected. Each protocol pin must map to a distinct typed project
wire alias and an actual top-level port in the role's required direction.
State mapping, reset polarity and source path validation follow the existing
project entry point. The [AXI binding](../examples/axi4lite/binding.json) contains:

```json
{
  "version": 1,
  "project": "project.json",
  "config": {"address_width": 8, "data_width": 32, "capacity": 1, "role": "subordinate"},
  "signals": {"awvalid": "bus_awvalid", "awready": "bus_awready"}
}
```

The short `signals` example above must be expanded to **all** names returned by
`signal_types(config)`; the checked example supplies the complete mapping.
Unmapped or aliased pins, wrong directions, unsupported widths and unknown
configuration fields are errors. History/counter registers belong to the
observer; they are never written into DUT state or accepted as simulator inputs.

```sh
python3 protocols/axi4lite_project.py search /my/project/axi.json --out /tmp/search --regression /my/project/failure.json
python3 protocols/axi4lite_project.py replay /my/project/axi.json --out /tmp/replay --regression /my/project/failure.json
python3 protocols/axi4lite_project.py stimulus /my/project/axi.json --out /tmp/scenario --inputs /my/project/inputs.json
```

`--out` must be fresh. `inputs.json` is an array of complete canonical external
input objects: initial reset, then nonreset edges. Search saves a regression only
for a reproduced failure and never overwrites a file. Replay checks source,
specification, bindings, library/oracle code and dependency identities. Changed
identities are rejected. A failure is replayed against the original property,
then the actual pinned Celox source simulation; an independent integer/snapshot
oracle checks those bus samples. Source behavior, not a chosen relational-spec
witness, determines the comparison values.

`model.json` is the generated native composition. `search.json`,
`scope-search.json`, `axi-contract.json`, simulation samples and the independent
oracle result retain the claim and its boundaries. Search through depth 1–32
reports `bounded_no_failure`, `reset_reachable_failure`, `scope_exceeded`, or
`unknown`; it does not establish unbounded induction. Capacity search uses the
same legal-counterpart prefix, separately from DUT obligations. Search alone does
not establish environment nonvacuity: supply a legal positive stimulus or a
separate cover. The examples require actual handshakes, not merely idle traces. A concrete
`stimulus` run reports environment violations explicitly. Exit 0 means the
request completed; inspect the status. Tool/mapping/divergence errors exit 2.

The Python document APIs default to **conditional guarantees**, not complete
trace acceptance. Generate the other objectives independently:

```python
from protocols.axi4lite import trace_document
checks = {goal: trace_document(config, objective=goal)
          for goal in ("guarantees", "environment", "scope")}
# For source documents, call bind(original_document, ..., objective=goal)
# separately on the original, uninstrumented document for each goal.
```

`environment` checks counterpart violations; `scope` checks capacity on a legal
counterpart prefix. Inspect these results separately from guarantees. Concrete
replay states also expose `axi_environment_bad` and raw `axi_scope_bad`; raw
capacity overflow is recorded even outside a legal counterpart prefix.
`axi4lite_reference.check_trace(rows, config)` returns separate
`conditional_guarantees`, `environment`, and `capacity` dispositions. Its existing
aggregate `status` prioritizes a detected DUT fault, then invalid environment,
then exceeded capacity; only `sampled_prefix_passed` means this supplied prefix
is both legal and in scope. `environment_nonvacuity.checked` is explicitly false:
these trace dispositions do not claim a quantified environment or cover check.

A capacity overflow does not mask a DUT fault on that same edge. A counterpart
violation **does** end the conditional prefix on its own edge, so a coincident
DUT violation is outside that conditional obligation; trace-only `link` mode
checks both without counterpart assumptions. Previously detected DUT faults
remain recorded despite later counterpart violations. Native guarantee replay
stops at its first failure; inspect a separate objective or the complete concrete
trace oracle when examining later events.

### Accepted write address/strobe consistency

`write_address_strobe` checks Arm IHI 0022H A3.4.4 and B1.1.3 for the explicit
32/64-bit full-width Lite profile. Independent bounded queues retain accepted AW
address offsets and W strobes. The oldest entries pair when both exist, including
W-first and same-edge handshakes; pairing does not wait for B. A mask is legal
when no asserted lane is below `AWADDR modulo (data_width / 8)`. Aligned addresses
permit any mask, and all-zero or sparse masks are legal. An unmatched pending
channel does not establish a complete transaction and is not guessed.

This is manager-owned safety: manager/link guarantees check it, while subordinate
checks treat it as a counterpart requirement. The queues reset with the monitor;
oldest-entry consumption and simultaneous append preserve ordering. Existing
AW/W-to-B outstanding counts still define capacity independently. An unmatched
queue cannot exceed its outstanding count on a legal prefix, so capacity remains
a tool bound, not a strobe rule. Earlier failures remain sticky, including a
strobe fault on the same edge as capacity overflow. Native trace replay uses one
initial reset; the independent oracle additionally tests repeated reset epochs.

This does not prove subordinate byte-write effects, memory contents, or response
transaction origin. The conformance inventory retains separate unchecked entries
for those claims. Its `next_evidence` fields describe phase-aware reset release,
explicit optional-signal/default profiles, and independently bound response
origins. None of those plans is an implemented compliance claim.

### AXI limits and validation

The gate runs the existing Celox replay tests plus positive source scenarios,
manager/subordinate mutants, replay and stale-identity controls. Hand-authored
expected traces cover every supported rule, both roles and widths, simultaneous
pop/push at capacity, indefinite stalls, and reset epochs. Exhaustive short write
sequences and fixed-seed full-channel sequences compare the generated native
monitor to a separately implemented procedural oracle. Actual source runs cover
AW-first, W-first, simultaneous, continuous traffic and response backpressure;
mutant stimuli also run on correct RTL with explicit expected transfer counts.

The sampled contracts alone leave unchecked: input-to-output combinational paths, VALID dependence on READY or
other channels, asynchronous reset assertion/deassertion and reset-release VALID
timing, transaction-identity ordering and functional address/data/strobe behavior,
fairness/liveness, X/Z, CDC, bursts, IDs and other AXI variants. Source replay uses
a single positive-edge clock, ordinary bit synchronous reset, scalar two-state
ports, one initial reset and the pinned Celox/Veryl subset. Standalone
`trace_document(config)` / `bind(...)` provide the generated contract API;
`axi4lite_reference.check_trace(...)` accepts full sampled traces, including
explicit reset epochs. Neither constitutes an asynchronous-reset timing check.

This experimental tooling is provided as-is, with no AXI certification or fitness
claim. Review its assumptions and independently validate it for your use. This
notice does not change the repository's licensing terms or provide legal guarantees.

## Native source structural contracts

A v3 component may require absence of a combinational path independently of its
behavioral relations. This is **new source-graph checking**, not a temporal or
physical timing proof. See the complete [native example](../examples/no_comb_path.hwv)
and [Veryl source](../examples/no_comb_path.veryl).

```text
component Isolation {
  init true;
  invariant true;
  steps { tick = true; }
  structure {
    no_comb_path isolated { from i.a; to o.y; }
  }
}
```

Endpoints reference declared shared inputs (`i.a`) or observations (`o.y`) for
their names/types. `implementation` binds them **separately** to actual source
signals, not to the behavioral state-only observation expressions:

```text
endpoints { i.a = a; o.y = y; }
```

Mappings are plain typed signal paths, including named child ports. Direct scalar
parent/child port connections denote the same source net and are traversable in
both directions, including when a contract starts at a child input. Assignment
data/control edges remain directed; shared assignment inputs do not make their
destinations aliases. Register cuts remain non-traversable. Expressions,
undeclared logical endpoints and direction/width mismatches are rejected. A
hardware endpoint binding does not itself prove that a behavioral abstraction
matches the same physical signal. Existing behavioral refinement obligations
remain separate. Nested v3 compositions collect contracts from their selected
components, retain qualified rule names, and apply the existing duplicate-member
checks. V4 structural declarations are not implemented and are rejected.

With the pinned frontend and release binaries built, run:

```sh
python3 conformance/celox-replay/structure_project.py examples/no_comb_path.project.json --out /tmp/no-comb-path
```

The source command checks structural obligations only. To check the native
behavioral model and structural obligations together using that artifact:

```sh
cargo run --release --bin hwverify-rs -- examples/no_comb_path.hwv --structural-artifact /tmp/no-comb-path/structural-graph.json --out /tmp/combined
```

This checks the supplied native behavioral model; it does not by itself establish
that its reset/next abstraction matches the RTL. Source lowering/replay provides
that separate connection. Structural violations produce `structural_contract_failed`;
unbound/unsupported structural checks keep the combined result `unknown`.

The small manifest names `version: 1`, `top`, a nonempty list of source filenames,
and `specification`; files must remain within its directory. The command compiles
the original source with the existing pinned frontend, extracts a graph from the
**original source tokens**, cross-checks every variable and instance path against
typed frontend reflection, and runs the generic Rust `hwverify-structure` checker.
The source parser is part of the trusted frontend boundary. The Rust checker
validates and traverses its artifact; it does not independently recompile RTL or
authenticate an arbitrary caller-supplied graph.

The graph contains whole-variable data, control and hierarchy-connection edges.
Assignments in supported positive-edge `always_ff` blocks produce explicit
sequential cuts. Data/control paths through combinational assignments remain even
in `a ^ a`, masked expressions, overwritten assignments or constant branches.
Thus `violated` means a path in this conservative **source-variable graph**; it
need not imply a sensitizable path or a path in optimized synthesized hardware.
No absence claim is inferred from simplified behavioral expressions or SAT
support sets. A register cut does not establish independence from historical
READY values or compliance with transaction-offer dependency requirements.

Supported source subset: scalar bit/logic widths 1–64, ordinary assignments,
expressions without function calls/casts/selects, `always_comb`, block `if/else`,
positive-edge `always_ff` with an ordinary clock, and fully named scalar module
connections. Parent/child aliases and sequential boundaries are retained.
Generics, generate blocks, arrays/slices, interfaces, external/opaque modules,
functions, casts, loops, case statements, `else if`, and typed/asynchronous reset
constructs are unsupported. Source budgets are explicit. Any unsupported construct
or incomplete reflection coverage makes the **whole artifact unsupported**;
there is no assumption that a missing subgraph is isolated.

Results are `verified`, `violated`, `unsupported`, `unbound`, or
`invalid_or_tool_error` (`not_requested` if no contracts are declared). Only `verified` completes the requested structural
checks. Path witnesses identify source files/lines and edge kinds; reports retain
native declaration locations, source identity, extractor identity, coverage and
sequential cuts. The CLI exits 0 for verified, 1 for violated, 3 for incomplete
checks and 2 for invalid/tool errors. An ordinary `hwverify-rs` or editor proof
without the source artifact reports the structural obligations as unbound and
keeps the overall result `unknown`; the editor provides a blocking declaration
location diagnostic. Behavioral replay rejects documents with retained structural
contracts, rather than silently ignoring them.

The AXI source entry point now generates a separate native structural composition
for every protocol input/output pair and uses this same checker. It emits
`structure-model.json`, `structural-graph.json` and `structural-result.json` beside
the sampled evidence. A path reports `structural_violation`; incomplete structural
coverage prevents a successful combined result. Clock/reset physical timing and
registered wait-on-READY remain separate unchecked obligations. The regression
suite deliberately includes a registered READY wait that passes this structural
check: it must not be mistaken for proof of temporal offer causality.

The [machine-readable AXI conformance inventory](../protocols/axi4lite-conformance.json)
records exact sections, profiles, tests and outstanding gaps. It is explicitly
partial, pending an independent completeness audit. Address/WSTRB consistency is checked for accepted pairs, including W-first
traffic; response correspondence still needs independent transaction-origin
evidence, not response counters alone.
Reset release, optional/default signal profiles and memory-versus-peripheral
requirements remain visible gaps. No full AXI compliance claim is made.
