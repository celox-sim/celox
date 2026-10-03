# ポートを持つ仕様と明示的な合成（0.9 / JSON version 4）

各仕様に局所的な入出力を宣言し、合成時にポートを明示的に対応付ける。
`operation` は一回の論理的操作を許す関係であり、クロックイベントや
`always_ff` のような逐次実行ブロックではない。
既存のversion 2 `design`とversion 3の共有グローバルポート形式は変更しない。
後者は [relational specifications](specifications.md) を参照。

0.9の現在値/prime、expectation、入力と実行の量化、短い操作呼出し表記は
[期待と量化trace](expectations.md)を参照。この文書のpositive/negative例は従来の存在検査を保つ。

## まず読む例

- [scoped_budgeted_counter.hwv](../examples/scoped_budgeted_counter.hwv): 算術と予算の仕様を明示配線し、有限例と実装bindingを検査
- [scoped_dual_operator.hwv](../examples/scoped_dual_operator.hwv): 独立したleft/right操作を選び、片方ずつ・両方同時・両方停止を検査。実装の二つのselectorも同時にtrueになれる
- [scoped_independent_counters.hwv](../examples/scoped_independent_counters.hwv): 同じCounterを二回使い、入れ子の合成でポートを改名。状態は独立、操作は省略形で同期。`pair.left` / `pair.right` の状態をbinding
- [scoped_contradictory_outputs.hwv](../examples/scoped_contradictory_outputs.hwv): 同じ出力を別々の仕様が制約し、合成後の正例だけが失敗する意図的な例

各 `.hwv` と同名の `.json` は同じversion 4文書を表す。

```sh
cargo run --locked -- examples/scoped_budgeted_counter.hwv --check --out /tmp/scoped-check
cargo run --locked -- examples/scoped_budgeted_counter.hwv --out /tmp/scoped-budgeted --z3 /path/to/z3
cargo run --locked -- examples/scoped_independent_counters.hwv --out /tmp/scoped-pair --z3 /path/to/z3
cargo run --locked -- examples/scoped_dual_operator.hwv --out /tmp/scoped-dual --z3 /path/to/z3
cargo run --locked -- examples/scoped_contradictory_outputs.hwv --out /tmp/scoped-conflict --z3 /path/to/z3 # exit 1
```

`--check` / `--emit-json` は完全な構文・名前・型検査を行うが、証明義務は実行しない。

## 仕様の局所インターフェース

```
specification "Counter"

spec Counter(input amount: bv<4>, output count: bv<4>) {
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  operation add { expect value' == value + amount; }

  example add_two {
    expect positive;
    initial { count = 0u4; }
    trace { add { inputs { amount = 2u4; } observe { count = 2u4; } } }
  }
}
```

`specification "名前"` の後に外側の波括弧やセミコロンは不要。
`spec Counter(...)` の括弧内がその仕様だけのインターフェースで、引数がなくても `()` を書く。
別の仕様のポートや文書全体の暗黙のグローバル変数は参照できない。

- `amount` / `i.amount`: 今回の操作の入力
- `count` / `o.count`: 現在の出力
- `count'` / `no.count`: 次の出力
- `value` / `s.value`: 現在の非公開状態
- `value'` / `n.value`: 次の非公開状態

裸のポート名と `i.` / `o.` の参照はいずれもその仕様の宣言範囲で解釈する。
`init` / `invariant` は現在の非公開状態と出力だけを参照する。
`operation add = ...;` は入力・現在／次状態・現在／次出力を使えるBoolean関係。
更新したい状態の関係を明示し、省略された状態を暗黙の保持とは解釈しない。
`operation add = true;` はその操作を無制約にし、保持は `n.value == s.value` と書く。

`output` は関係上の観測量であり、ハードウェアの単一driverを宣言する語ではない。
複数の仕様が同じ出力を制約でき、制約が矛盾すればその振る舞いは存在しなくなる。
型・固定幅のwrap・式の演算子は [language guide](language.md) と共通。

## ポートの対応付けと合成

```
composition Budgeted(input amount: bv<4>, output count: bv<4>, output remaining: bv<4>) {
  use Counter(amount: amount, count: count);
  use Budget(amount: amount, remaining: remaining);
  operation add = actions(Counter.add, Budget.add);
}
```

`use Counter(...)` はCounterという名前のインスタンスを作る。
対応付けの左辺は子のポート名、右辺はこのcompositionのポート名。
全ての子ポートをちょうど一度対応付け、型と方向を完全に一致させる。
子inputは親inputへ、子outputは親outputへ接続する。
未宣言ポート、幅の違い、方向の交差、未接続、重複指定はエラー。
右辺に定数・式・内部wireを書いたり、出力を別の子の入力に接続したりはできない。

### 操作の公開と独立した実行

`operation add = actions(Counter.add, Budget.add);` は合成の公開操作 `add` を定義し、
指定された二つの子操作の関係を一つのステップで論理積にする。
右辺は直属の子インスタンス名とその公開操作名（`alias.operation`）の空でない一覧。
子がcompositionなら、その公開操作を再帰的に葉specの操作まで展開する。
親は子と別の公開操作名を選べるが、子の内部パスを直接飛び越えて参照できない。

```
composition Pair(input a: bv<4>, input b: bv<4>, output x: bv<4>, output y: bv<4>) {
  use left: Counter(amount: a, count: x);
  use right: Counter(amount: b, count: y);
  operation left = actions(left.add);
  operation right = actions(right.add);
}
```

このPairは `left` と `right` を別々に、または同時に実行できる。
公開操作の宣言は許可するグループを定義するもので、自動的に実行を開始しない。
一つのtraceフレームがそのとき選ぶ公開操作集合を指定する。
二つの操作があるからといって、各部分集合の別名をあらかじめ列挙する必要はない。

グループは集合として扱う。同じ葉インスタンスの同じ操作に複数の経路で到達しても一度だけ実行する。
異なる葉の操作は同時に選べる。同じ葉の異なる操作を同時に選ぶ組合せは互換性がなく、そのtraceはUNSATになる。
これは構文・名前のエラーとは区別し、negative例として不許可を確認できる。
独立性はインスタンスの完全なパスで判断するので、同じspecを使った `left` と `right` は別の葉になる。
明示グループがある場合、子同士の操作名集合が異なっていてもよい。

composition内で操作宣言を全て省略すると、同名操作で全メンバーを同期する省略形になる。
この場合だけ全メンバーが同じ空でない操作名集合を持つ必要があり、各操作は全子の同名操作をまとめる。
明示グループを一つでも書いた場合、他の操作を暗黙に追加することはない。
空のグループ、未知の子／操作参照は拒否する。

### 繰り返し利用と入れ子

```
composition Pair(input a: bv<4>, input b: bv<4>, output x: bv<4>, output y: bv<4>) {
  use left: Counter(amount: a, count: x);
  use right: Counter(amount: b, count: y);
}
```

`left` / `right` はインスタンス名。同じ仕様を使っても非公開状態は独立であり、
同じ入力・出力へ接続した場合にだけその共有値を通じて制約が結び付く。
同一composition内のインスタンス名は重複できない。
`use pair: Pair(...)` のようにcompositionを使うと、ポート対応を入れ子に合成する。
非公開状態の葉の識別名は `pair.left` / `pair.right` となる。
未知の参照先と循環は展開前に拒否する。
未使用の宣言も全てsolver起動前に検査する。展開の上限は入れ子64段、
一対象4096葉、全文書の展開先を合計して16384葉。超過は検証成功とはせず入力エラーにする。

## 正例・負例の範囲

各 `spec` / `composition` に `example name { ... }` を直接置く。
`initial` / `observe` はその対象のoutput名、traceの `inputs` はその対象のinput名を使う。
trace中の `add { ... }` は操作の出現で、`operation` 宣言とは別の構文。
同名の操作を何度書いても別のステップとして順番を保持する。

```
trace {
  left { inputs { a = 2u4; b = 7u4; } observe { x = 2u4; y = 0u4; } }
  actions(right) { inputs { a = 9u4; b = 3u4; } observe { x = 2u4; y = 3u4; } }
  actions(left, right) { inputs { a = 4u4; b = 5u4; } observe { x = 6u4; y = 8u4; } }
  actions() { inputs { a = 1u4; b = 1u4; } observe { x = 6u4; y = 8u4; } }
}
```

`left { ... }` と `actions(left) { ... }` の意味は同じで、前者は従来の単一操作表記。
`actions(left, right)` は同じ一歩で両方を選び、二つの逐次ステップにはしない。
`actions()` は何も選ばない一歩。順序付きtraceフレームの境界が論理ステップを定める。

**非選択の葉は非公開状態だけを保持する。** 現在／次の全葉invariantはどのフレームでも必要。
公開出力について一律の保持や暗黙のdriver所有権は課さない。
例えばこのCounterの出力は `count == s.value` なので非選択時にも保持されるが、
出力がinvariantで一意に決まらない仕様では、全葉が非選択でも出力が変わり得る。
二つの子outputを同じ親outputへ接続した場合は両方の関係を満たす必要があり、
同時に選べる葉でも出力制約の衝突でtraceがUNSATになり得る。

positiveは指定に合う有限実行が少なくとも一つ存在すること、negativeは存在しないことを要求する。
入力・出力の省略値と非公開状態は存在量化され、0等では補わない。
`initial {}` / `trace {}` は空でも明示する。
trace stepの `inputs` / `observe` は省略でき、空の部分指定として扱う。

**各宣言の例は、その宣言自身のインターフェースについて一度だけ検査する。**
インスタンス化しても例を継承・複製せず、子の例が通ったことを合成の成功とはみなさない。
必要な合成後の振る舞いはcomposition自身にも例を書く。
`scoped_contradictory_outputs` ではOneとTwoの単体例は通る一方、
両方を同じ出力へ接続したcompositionの正例が失敗する。

SATのwitnessを全状態・全入出力で取得して再確認し、UNSATは隠れた補完を全て排除する。
UNKNOWNやsolver異常を成功にはしない。有限例の成功は仕様の完全性、任意長の実行、
deadlock freedom、liveness、実装が全ての正例を実現できることの証明ではない。
負例だけが通っても、許される実行が存在することは示せない。

## 実装と状態写像

実装は一つのcompositionを選び、その入力を同名・同型で宣言する。
resetやaccept制御のような追加の入力も実装内で明示する。
この版の `implementation` はポートシグネチャを持たず、個別 `input` 宣言を使う。

```
implementation {
  composition Budgeted;
  input rst: bool;
  input go: bool;
  input amount: bv<4>;
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
    output count = s.count;
    output remaining = s.remaining;
  }
}
```

実装の `operations { add = ...; }` はBoolean selectorであり、specの
`operation add = ...;` は関係。名前は対応するが役割は異なる。
`reset` / `next` / `wires` / selectorの既存の機械意味論は変更しない。
resetはactive-high・同期・最優先であり、各next式は更新前の状態を読む。

bindingは全ての葉インスタンスの全状態と、選択したcompositionの全outputを
過不足なく写像する。入れ子は `bind pair.left { value = s.left; }` と書く。
写像式は実装の現在状態だけに依存し、現在入力やwireへの依存は許さない。
この版はresetによる初期条件・invariantの確立、各ステップのinvariant維持、
選択された各葉操作の関係、非選択の各葉の写像状態保持を検証する。
全体で操作がない場合も同じ葉ごとの規則を使い、公開出力の一律保持は課さない。

selector同士は、異なる葉だけに作用する場合や同じ葉の同じ操作を重複して選ぶ場合、
同時にtrueでよい。同じ葉の異なる操作を選ぶ組合せだけについて排他性を証明する。
排他性は検証する結論であり、他の義務を自明にするための仮定には使わない。
`scoped_dual_operator` の `left = i.go_left; right = i.go_right;` は
両方trueでも受理され、両方falseなら二つのCounterの非公開状態が保持される。
これは葉ごとの選択条件で検証し、全selector部分集合の列挙をしない。

与えた決定的状態写像によるsafety / stuttering refinementで、到達可能性を絞り込まず検査する。
常にstutterする実装も通り得る。進行・公平性・停止性や、仕様の全振る舞いの実装可能性は要求しない。

## version-4 JSON

完全な例は [scoped_budgeted_counter.json](../examples/scoped_budgeted_counter.json)。
式・型・有限例の配列形式はversion 3と共通で、参照名だけを各局所範囲で解釈する。

- ルート: `version: 4`、`kind: "specification"`、`specs`、`compositions`
- 各spec: `inputs`、`outputs`、`state`、`init`、`invariant`、`operations`、`examples`
- 各composition: `inputs`、`outputs`、`instances`、`examples`。任意の `operations` は公開操作名→直属の子の `alias.operation` 参照配列
- 各instance: `target` と `connections`。instance名は `instances` のキー、connectionsは子ポート名→親ポート名のobject
- 任意のimplementation: `composition`、`inputs`、`reset_input`、`state`、`reset`、`next`、`operations`、`binding`
- implementationのbinding: `states` と `outputs`。statesは葉インスタンスパス→状態写像、outputsはポート名→状態のみの式

任意のキーはルートの `name` / `implementation`、compositionの `operations`、implementationの `wires`。
それ以外のキーは必須で、空のmapもJSONでは `{}` と明示する。
specの `operations` は操作名→Boolean関係、implementationの同名mapは操作名→Boolean selector。
各exampleは `expect`、`initial`、`trace` を持つ。各traceフレームは `inputs`、`observe` と、
`operation: "left"` または `actions: ["left", "right"]` のどちらか一方だけを持つ。
`actions: []` はidle。出力ポートの値指定には既存のexampleキー `observe` を使う。
JSON printerは単一操作／集合の元の表現を保ち、配列を勝手に並べ替えない。

```
"operations": {"left": ["left.add"], "right": ["right.add"]}
```

このcompositionのmapを省略すると同名同期の省略形になる。空mapや空グループでの代用はできない。
重複JSONキー、重複インスタンス名、未定義の参照先を拒否する。

## 旧形式との境界と移行

シグネチャ付きの `spec Name(...)` / `composition Name(...)` がある文書はversion 4へloweringする。
旧 `component Name { ... }`、グローバルのinput / observation / operation、
シグネチャなし `composition Name { members compose(...); ... }` はversion 3のまま。
同一文書内で両形式は混在できない。旧version 2/3のJSON・例・検証の意味は変わらない。

version 3からversion 4への移行では、各仕様のインターフェースと接続先を明示的に決める。
グローバルポートや同一componentの同一性を機械的に分割する意味変換は行わない。
`scripts/json_to_hwv.py` は与えたschema versionを保つprinterで、v3を自動的にv4へ変換するものではない。
新しい宣言語は文脈で解釈し、グローバル予約語は増やさない。

```sh
python scripts/json_to_hwv.py examples/scoped_dual_operator.json /tmp/scoped-dual.hwv --infix
cargo run --locked -- /tmp/scoped-dual.hwv --emit-json /tmp/scoped-dual.json --out /tmp/scoped-dual-check
```

処理系の版は0.8、JSON schema versionは2 / 3 / 4であり別の番号。
parser、lowering、インスタンス展開、型検査、義務生成とZ3は引き続き信頼対象で、
この追加によって処理系全体の健全性が形式的に証明されたわけではない。
