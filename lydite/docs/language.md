# lyd: 状態遷移と検証契約を書く言語（0.9）

0.7の`specification`文書（関係仕様の合成、正例・負例、限定的binding）は
[relational specifications](specifications.md)を参照。このページの
version 2 `design`言語・意味論は互換性を維持している。
0.9のprime・名前付き期待・量化traceは [expectations and traces](expectations.md) を参照。
局所ポート、明示配線、独立した操作集合を持つversion 4は
[scoped specifications](scoped-specifications.md)を参照。

`.lyd` は parol 5.0.2 で生成するパーサを使う。文法は
`crates/lydite-syntax/src/lyd.par`。生成 AST を solver のデータ構造にせず、ソース位置付きの
フロントエンドを経て、JSON 入力と共通の型付き `lydite_ir::Design` を作る。
JSON は既存の入出力・互換検査形式として残る。

## まず読む例

- [memory_increment_readable.lyd](../examples/memory_increment_readable.lyd): 手書きの短い例。
  型付きリテラル、if/else、配列添字、演算子を使い、任意位置のメモリを3回更新する
- [auto_array_sum.lyd](../examples/auto_array_sum.lyd): 既存の総和契約を移行した例
- [array_sum.lyd](../examples/array_sum.lyd): 手動分割を指定する総和
- [memory_increment.lyd](../examples/memory_increment.lyd): 旧 JSON の木構造を明示した移行版

手書き版と移行版のメモリ例は、同じ JSON 文書へ正確に変換される。余分な scalar は
分割ヒューリスティクスを試すためのもので、メモリ設計の必須要素ではない。

```
cargo run -- examples/memory_increment_readable.lyd --check
cargo run -- examples/memory_increment_readable.lyd --emit-json /tmp/memory.json
cargo run -- examples/auto_array_sum.lyd --out /tmp/sum --z3 /path/to/z3
cargo test --workspace --locked
cargo install --path ../crates/lydite --locked
```

`--check` と `--emit-json` は構文・名前・型等の検査だけを行い、証明済みとは表示しない。
通常実行でも、この検査が完了する前に Z3 を起動しない。拡張子 `.lyd` を自動判定し、
`--format lyd` / `--format json` で明示できる。既存の JSON 実行形式は維持する。

## 言語の構成

文書の先頭に `design "名前"` と書き、その後に宣言を並べる。
ヘッダの末尾にセミコロンは付けず、文書全体を囲む波括弧も不要。

- `input rst: bool;`, `input data: mem<8, 8>;`: 共有入力
- `reset_input rst;`: 同期・active-high・最優先 reset の入力名
- `spec { ... }`, `impl { ... }`: 各機械
- `binding 式;`: 抽象状態と実装状態の対応関係
- `commit 出力名;`, `can_step 出力名;`: impl / spec の Boolean 出力を指定
- `hold_when 式;`: 保持が必要な条件（省略可）
- `progress { enabled 式; rank 式; }`: 非commitステップの進行条件
- `contract { ... }`: プログラムの契約（省略可）

機械内には `state count: bv<4>;` のような状態宣言と、`reset`、`next`、
`outputs`、任意の `wires` ブロックを置く。代入はブロック内に `name = 式;` と書く。
reset/next は全状態にちょうど一回ずつ値を与える。代入順による逐次実行はしない。
全ての next 式は更新前の状態を読む。省略代入を暗黙の保持とは解釈しない。

名前は ASCII の英数字と `_`（先頭数字不可）。`if` / `else` / `true` / `false` は予約語。
`design` / `specification` は文書先頭で種類を指定し、識別子の位置でも使用できる。
`input` / `state` / `parameter` 等の宣言語も文脈で解釈し、新たな予約語にはしない。
例えば `input state: bool;` は `state` という名前の入力を宣言する。
参照の `.` は名前空間の区切り。
`i.x` は入力、機械内の `s.x` はその機械の状態、`w.x` は wire。
binding では `spec.x` / `impl.x`、プログラム契約では `s.x` / `p.x` を使う。
利用できる名前空間は役割ごとに制限する。wire は循環のない組合せ式である必要がある。
同じ宣言、代入、section、property の重複は上書きせずエラーにする。

## 型と式

- `bool`
- `bv<N>`: 1–64 bit の語
- `mem<A, W>`: A-bit アドレスから W-bit 語へのメモリ
- `255u8`: 値255、幅8。幅を越える値は既存 IR と同じ剰余解釈
- `bv(8, -1)`: 8-bit の全ビット1。負数は signed 64-bit 範囲内
- 裸の数値は bv / extract / extension / range などの数値引数にだけ使う
- `true`, `false`
- `if 条件 { 式 } else { 式 }`: 両枝は同じ型
- `memory[address]`: `read(memory, address)` と同じ
- `write(memory, address, value)`: 更新されたメモリ値を返す。式中で状態を変更しない

通常の例:

```
count = if s.count != 0u4 { s.count - 1u4 } else { s.count };
mem = write(s.mem, s.address, s.mem[s.address] + 1u8);
```

優先順位は、強い方から以下。二項演算は左結合。

1. 呼出し、添字、括弧
2. `!`（Boolean否定）、`~`（語の反転）、数値引数の単項 `-`
3. `*`
4. `+ -`
5. `<< >>`
6. `== != < <= > >=`
7. `&`
8. `^`
9. `|`
10. `&&`
11. `||`

比較の連鎖は認めない。`a < b < c` はエラーで、`a < b && b < c` と書く。
`< <= > >=` は unsigned。signed 比較には `slt(a,b)` / `sle(a,b)` を使う。
`& | ^ ~` は固定幅の語、`&& || !` は Boolean。型をまたぐ暗黙変換は行わない。
単項 `-` は裸の整数引数に限定する。語の減算は `0u8 - x` または `sub(0u8,x)` を使う。

IR の演算は呼出し形式でも書ける:
`bv`, `const_mem`, `extract`, `zext`, `sext`, `not`, `bnot`, `and`, `or`,
`xor`, `implies`, `eq`, `ne`, `ite`, `add`, `sub`, `mul`, `band`, `bor`,
`bxor`, `shl`, `lshr`, `ult`, `ule`, `slt`, `sle`, `read`, `write`, `concat`。
`const_mem(address_width, value)` の語幅は value の型から決まる。
`extract(high, low, word)`、`zext(amount,word)`、`sext(amount,word)` の範囲も検査する。

## プログラム契約

`contract` 内には `parameter name: 型;`、`pre`、`invariant`、`terminal`、
`post`、`rank` を置く。意味は既存 [JSON schema](schema.md) と同じ。
`precondition` / `postcondition` は `pre` / `post` の別名。

自動分割は既定。`partition auto;` または `partition none;` で明示できる。
手動分割は次の形式で、min/max は両端を含む。

```
split {
  pc = range(s.pc, 0, 2);
  index = range(s.idx, 240, 255);
}
```

`cases { name = Boolean式; }` も使える。自動指定と手動指定の併用は禁止する。
`range` は split 専用であり、通常の語の演算ではない。

## 既存ソースの互換性と移行

0.8の個別宣言表記はversion 2 / 3 JSON と検証の意味を変更しない。
局所ポート・明示配線・独立操作の拡張は別のversion 4文書で利用する。
旧 `design "名前" { ... }` / `specification "名前" { ... }` の外側の波括弧、
`inputs { name: 型; }`、`state { name: 型; }`、
`parameters { name: 型; }` は引き続き使える互換表記。
旧コンテナと個別宣言を混ぜても、同じ名前を二度宣言することはできない。
関係仕様の個別宣言と互換表記は [relational specifications](specifications.md) を参照。

入力・状態・契約パラメータの宣言が一つもない場合、DSLではその宣言群を省略できる。
lowering は対応する JSON の `inputs` / `state` / `parameters` に `{}` を補う。
JSON入力ではこれらの必須キーを省略できない。省略しても意味検査は緩まらず、
例えば `reset_input` に指定するBoolean入力は実際に宣言する必要がある。
`reset` / `next` / `outputs` 等の必須ブロックは従来通り必要。

JSONから移行する場合は、式木とリテラル値を保つprinterを使える。

```sh
python scripts/json_to_lyd.py examples/array_sum.json /tmp/array_sum.lyd
cargo run --locked -- /tmp/array_sum.lyd --emit-json /tmp/array_sum.json
```

printerはv2/v3/v4それぞれの新表記を出力し、schema versionをまたぐ意味変換は行わない。
束縛変数はprefixなしで出力する。出力名と衝突するversion 4の束縛変数は、
宣言と参照をまとめて未使用名へ変更する（量化順序・型・意味は保持）。
受理・型検査はRust側のparser/validatorが行う。
`--infix` は対応する演算だけを括弧付き中置表記にする。

## 診断と信頼境界

構文、重複、未定義名、型の混用、幅の不一致、wire の循環、不正な契約等は失敗にする。
parol がエラー回復を行っても、エラーのあった入力を成功として実行しない。
位置は UTF-8 byte offset と、1始まりの行・Unicode scalar単位の列を保持する。

ライブラリでは `parse_document(source, filename)?.validate()?` で型付き Design を得る。
`ParsedDocument` は編集可能な構文側のデータなので、それ自体を検証済み Design と
見なしてはいけない。Design の生成時には必ず全体を検査する。

この版では module/import、ユーザー関数、型推論、generic、仕様からの実装合成は提供しない。
パーサと lowering も信頼対象であり、一般的なコンパイラ正当性の形式証明はしていない。
選んだ例で、JSON・DSL の文書と、生成される SMT 義務・検証結果の一致を検査する。

## Native lemma proposals

A `design` can contain a `proof { ... }` block. A separate
`lemmas "Name" { ... }` module can be attached with `--lemmas FILE.lyd`.
Both forms lower to the shared typed candidate API and require fresh checked
proofs; they do not add assumptions to the model. The surface supports typed
`forall`, direct variable names, `impl.x'`/`spec.x'`, guarded `lemma` proposals,
explicit dependencies, and `use`. `--check` validates candidate names and widths
without proving claims. See the [native authoring guide](lemmas.md)
and [complete counter example](../audit/lemma_candidates/counter.lyd).
