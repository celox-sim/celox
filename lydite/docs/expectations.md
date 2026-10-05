# 期待する関係と、量化を明示した有限trace（0.9.1）

`operation` は実行手順ではなく、許す現在／次状態の関係を書く。
`example` はその関係の有限traceを、入力値と実行について指定した量化で検査する。
この二つは別の役割であり、どちらもクロックイベントの宣言ではない。

## 現在値と次の値

```
spec Counter(input amount: bv<4>, output count: bv<4>) {
  state value: bv<4>;
  init value == 0u4;
  invariant count == value;
  operation add {
    expect value' == value + amount;
  }
}
```

scoped `spec` の関係式では、裸の状態名 `value` は現在値、`value'` は次状態を表す。
出力も `count` は現在値、`count'` は次の出力。ASCIIの `'` を一つだけ付ける。
`invariant` でも裸の名前は現在値のままで、操作内だからといって意味を切り替えない。
次状態のinvariantは検証器が別途同じ関係を次フレームに適用する。

入力 `amount` は今回の操作で与える値であり、`amount'` はエラー。
`value''`、`s.value'`、`(value)'`、曲がった引用符 `value’` も受理しない。
メモリでは `memory'[address]` は次のメモリを読み出す。

互換表記 `s.value` / `n.value` / `i.amount` / `o.count` / `no.count` は引き続き使える。
状態とポートが同名の古い仕様も明示prefixなら有効だが、曖昧な裸の参照は拒否する。
例えば状態 `count` と出力 `count` が同居するなら `s.count` と `o.count` を使う。
`init` / `invariant` は現在状態と現在出力だけを参照でき、入力・次状態・次出力を拒否する。
この制限は名前付き期待の間接参照にも適用する。
version 2の機械や実装のreset/next/bindingに、この裸名・prime規則を持ち込まない。

## 名前付きの期待とAND/OR

```
expectation grows { expect value' == value + amount; }
expectation holds { expect value' == value; }
expectation permitted {
  any {
    all { expect amount <= 2u4; expect grows; }
    all { expect amount > 2u4; expect holds; }
  }
}
operation add {
  expect permitted;
  expect count' == value';
}
```

- `operation` / `expectation` の直下に並んだ条件はAND
- `all { ... }` は子条件のAND、`any { ... }` は子条件のOR
- 入れ子を保つ。上の例は `(amount <= 2 AND grows) OR (amount > 2 AND holds)` を要求する
- 一つの `expect` に `grows || holds` のような通常のBoolean式も書ける
- 各期待はBoolean関係。状態更新や順序付き手続きとして実行しない
- 空のoperation/expectation/all/anyは拒否する。意図的な無制約は `expect true;`、不可能な関係は `expect false;`
- 省略した状態や出力には保持を補わない。保持が必要なら `expect value' == value;` と書く

名前付き期待は一つの `spec` 内だけで利用でき、先に参照して後で定義してもよい。
引数、可変束縛、他specからの参照、動的なマクロは持たない。
状態・input・outputと同名の期待、重複定義、未使用を含む循環や型エラーを拒否する。
同じspecを二回使っても、各インスタンスの局所状態で解釈する。
展開前に全依存グラフを検査し、依存64段、all/anyの入れ子64段、
一specの関係展開262144ノードの上限を設ける。上限超過は入力エラーであり、成功にしない。

`operation add = relation;` は互換表記として残る。
名前付き期待はfrontendで既存の関係に展開され、独立した操作やsolverの仮定を追加しない。
primeとexpectationだけの変更ではJSON version 4のschemaやaction集合の意味は変わらない。
例は [expectation_counter.lyd](../examples/expectation_counter.lyd)。

## 一回の操作を読みやすく書く

```
example every_input {
  forall a: bv<4>;
  execution forall;
  initial { count = 0u4; }
  trace {
    add(amount: a) => count == a;
    add(amount: 1u4) => count == a + 1u4;
  }
}
```

`add(amount: a) => count == a;` は「この入力でaddを一回選んだ後の出力について、
右辺を検査する」と読む。量化の意味は外側の `execution` と入力量化で定まる。
ここで `count` は矢印後のフレームの出力であり、private stateは参照できない。
右辺は出力名（version 3では `o.count`）と、`forall` / `exists` で束縛した変数から成るBoolean式。
入力引数が空なら `add() => predicate;` と書く。
操作名が `actions` の場合だけは、action集合構文と区別するため
`actions { inputs { ... } ensure ...; }` というブロック表記を使う。
入力名の重複、未宣言の入力、型の違いを拒否する。

この表記は従来の一操作traceブロックに `ensure` を付けたものと同じ:

```
add {
  inputs { amount = a; }
  ensure count == a;
}
```

同時実行・非選択は従来の `actions(left, right) { ensure ...; }` / `actions() { ensure ...; }` を使う。
`observe { count = expression; }` は出力との等式を検査する互換表記で、
`ensure` と併記すれば両方を要求する。`initial` は開始状態に課す前提。
traceのフレーム順序は維持され、二つの操作呼出しを同時実行にはしない。

## 入力と実行を別々に量化する

`forall a: bv<4>;` / `exists b: bool;` を書いた順に、外側から内側へ束縛する。
最後の `execution` が、その値のもとで許される有限実行を量化する。
量化変数は宣言した名前のまま `a` のように参照し、必要な入力引数に明示的に渡す。
量化変数を宣言しただけでは、同名の入力ポートに自動接続しない。

```
forall a: bv<2>;
exists b: bv<2>;
execution forall;
```

この順序なら `b` は `a` に応じて選べる。
逆の `exists b; forall a;` では全ての `a` に使える一つの `b` が必要になる。
同じ量化の変数は `forall { a: bv<2>; flag: bool; }` とまとめてもよい。
重複名やshadowingは拒否し、入力量化はexecution/expectの前に書く。
束縛変数はそのexampleだけに局所的で、別のexampleには漏れない。
`initial`・入力引数・`inputs`・`observe` の値の式では、裸の名前はそのexampleの束縛変数だけを参照する。
`ensure` と矢印の右辺では、version 4の出力名も裸で参照できる。
束縛変数と出力が同名の場合、その名前を裸で書くと曖昧なのでエラーになる。
束縛変数に別の名前を付けるか、出力を意図するなら `o.count` のように明示する。
version 3の出力は従来通り `o.count` と書くため、束縛変数と同名でも区別できる。
旧表記の `q.a` は互換性のため引き続き受理するが、通常のソースでprefixは不要。
内部のJSONでは束縛参照を従来通り `q.a` として保持し、型・量化順序・SMTの意味を変えない。
JSON printerも裸の名前を出力する。version 4で出力と衝突する束縛名は、宣言とその参照だけを
未使用の名前（例: `count_value`）へ一貫して変更し、出力参照・量化順序・グループ・型は保持する。
JSONでのみ使える予約語や数字始まりの束縛名も、ソースで使える未使用名に変更する。
型は `bool` / `bv<N>` / `mem<A,W>` を使え、入力量化のグループは256個まで。
領域を有限値に列挙して検査する実装ではなく、型付きの量化をSMTへ渡す。

現在／次の状態・出力と、省略された入力値の全フレーム分を `H` とする。
`R` はinit/invariant、初期指定、操作関係、指定入力から成る有限実行の条件。
`P` は各ステップのobserve/ensureを全てANDした、検査対象の条件。
**期待結果 `P` は実行を選ぶ前提 `R` に混ぜない。**

- `execution exists`: `∃H (R ∧ P)`。期待に合う有限実行が一つはある
- `execution not_exists`: `¬∃H (R ∧ P)`。期待に合う有限実行が一つもない
- `execution forall`: `(∃H R) ∧ ∀H (R ⇒ P)`。実行が存在し、許される全実行で期待が成立する

記述順の入力量化prefixはこの式全体の外側に掛かる。
`forall` は実行不能な入力で空虚に成功しない。可実行性は期待値を外した別の問い合わせでも報告する。
`not_exists` は実行そのものが不可能でも真になり得るので、実行存在の主張ではない。

旧 `expect positive;` は `exists`、`expect negative;` は `not_exists` と同じ存在条件を保つ。
これらは矢印/ensureと併用でき、検査条件を失わない。
`execution positive;` / `execution negative;` は曖昧な新表記として認めず、旧形式には `expect` を使う。
一つのexampleにexecution/expectを二回書くことはできない。
入力量化を指定する場合は、空prefixでも新しいexecutionモードを明示する。
旧positive/negativeと `quantifiers` を組み合わせることは認めない。

未指定入力は `H` の一部なので、`execution exists` では良い補完を選べる。
`execution forall` では実際に許される実行の補完全てを検査するが、全ての生入力の組合せで
操作が可能だと保証するわけではない。全入力で可実行性も要求したいときは、
対象入力を `forall` で明示して引数に渡す。

この版は **入力量化prefix → 一つの最内実行量化** に限定する。
実行量化と入力量化を任意に交互配置する言語、オンライン戦略・因果性の合成、
無限実行のlivenessを実装したものではない。
`forall` も指定された有限長の検査であり、任意の長さの帰納的証明ではない。
初期条件は引き続き前提として扱う。ユーザーの意図した仕様の完全性は別の問題である。

## 実例と結果の確認

- [quantified_counter.lyd](../examples/quantified_counter.lyd): 全4-bit入力で二回の加算、wraparound、入力ごとに選べる補正値
- [quantified_counter_bad_order.lyd](../examples/quantified_counter_bad_order.lyd): 補正値を入力より先に選ぶため失敗する例
- [quantified_counter_impossible.lyd](../examples/quantified_counter_impossible.lyd): 実行不能な入力があり、右辺がtrueでもforallが失敗する例

```
cargo run --locked -- examples/quantified_counter.lyd --out /tmp/quantified --z3 /path/to/z3
cargo run --locked -- examples/quantified_counter_bad_order.lyd --out /tmp/bad-order --z3 /path/to/z3 # exit 1
cargo run --locked -- examples/quantified_counter_impossible.lyd --out /tmp/impossible --z3 /path/to/z3 # exit 1
```

量化式はZ3へ直接送り、元のSMT式と問い合わせ結果を保存する。
各問い合わせのtimeoutは10秒。量化経路では構造的UNSATカーネルを使わない。
UNKNOWN、異常終了、取得したwitnessの再確認失敗は成功にしない。
reportの `valid` は命題の真偽または未確定、`feasibility.holds` は期待値を外した実行存在、
`nonvacuous.holds` は同じ外側の選択で実行存在と検査条件を満たせるかを示す。
別々の存在量化問い合わせで異なる入力を選べる点を混同しない。
量化の外側にある選択関数・戦略を具体witnessとして抽出した、という主張はしない。

## JSONと移行

schema version 3/4のexampleに任意の `quantifiers` 配列を追加する。
各要素は `{"kind":"forall"|"exists","variables":{"name":type,...}}`。
`expect` は旧 `positive` / `negative` に加えて `exists` / `not_exists` / `forall` を受理する。
各traceフレームには任意のBoolean `ensure` を追加し、`inputs` / `observe` は従来通りmap。
明示的な空配列 `quantifiers: []` はDSLで `quantifiers {}` と表せる。

JSON printerは旧形式も扱い、入力量化の順序とグループ、式木、trace順序を保持する。
一つのoperationに一つのexpectを出力して、既存のBoolean木を勝手に平坦化しない。
名前付き期待の名はcanonical JSONに残らないので、自動で名前を再構成しない。
状態とポートが同名の旧JSONの裸ポート参照だけは `i.` / `o.` に明示修飾して曖昧さを解消する。
この場合は等価な参照綴りへの変換となり、文字列まで同じJSONへの往復ではない。
