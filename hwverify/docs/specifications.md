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
