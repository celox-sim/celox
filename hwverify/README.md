# hwverify-rs 0.3

Rust＋Z3による、ハードウェア向け状態対応チェッカー。小さなCPUを題材に、**実装の複数マイクロサイクルを、ISAの1ステップへ対応づける**ところまで実装した。

この版はRustで型付きIR・正規化・証明義務を構築し、SMT-LIBをZ3へ送る。Pythonチェッカーを呼ぶラッパーではない。Pythonファイルは実例の生成と独立監査にだけ使う。

現状は **Z3のUNSAT結果とRust側の変換を信頼する**。処理系自身に形式的な正しさの証明を付けたものではない。

## 0.3の追加

- ISA上で固定16要素のmod256配列和を帰納的に検証し、同じCPUのfetch/execute refinementへ接続
- 再利用可能な`program_contract`でpre/invariant/post/terminationと停止後のquiescenceを検査
- compact split hintsからRustが完全な場合分けを生成。ユーザーのlemma/simp指定は不要だが、invariant/rank/hintsの選択はまだ手動
- 旧CPUのopcodeは変更せず、別例でindexレジスタとADDX/IXJを追加
- 単一SMT義務の10秒timeoutも保存。成功例だけを示した結果ではない

詳細は[プログラム検証](PROGRAM-ja.md)、[メモリ正規化の形式化](proof/README-ja.md)。

## 実行

Rust/CargoとZ3実行ファイルを用意する。今回の検証環境はRust 1.98.1、Z3 5.1.0。

```sh
cargo test --locked
cargo run --locked -- examples/cpu.json --out results/cpu --z3 /path/to/z3
```

`Z3_BIN`でもZ3の場所を指定できる。Cargo依存はlock済み。処理系の入力は宣言的JSONで、仕様・実装・binding・commit・進行条件を与える。利用者に補題名や書換え順序を要求しない。

## CPUサンプル

- 8ビットPC、8ビットaccumulator
- データメモリ：256×8ビット
- プログラムメモリ：256×16ビット、reset時に与え、実行中は不変
- fetch → executeの2マイクロサイクル。resetを除き、stall時は実装状態全体を保持
- ADDI、LOAD、STORE、JZ、HALT、XORI、NOP
- 命令の上位3ビットがopcode、下位8ビットが即値。中間5ビットは未使用
- PCと演算は幅に応じてwrap。HALTもcommitし、PCを1増やして停止

これはCPUの**宣言的な実装モデル**。Veryl/SVで合成可能なCPUを新たに作った、あるいはそのRTLとの一致まで証明した、という結果ではない。

サンプル生成では命令のデータパス式を仕様と実装で共有し、主にfetch・保持・commitの対応を試している。独立したPython ISAオラクルによる具体実行でも補強したが、独立に書かれた最適化済みCPUのRTLを検証したものではない。

仕様側は1命令の意味を書く。実装側は命令をfetchして保持し、executeでcommitする。bindingはアーキテクチャ状態の対応に加えて「execute中の保持命令はprogram[PC]」を表す。

## 検査する義務

1. reset後にbindingが成立
2. commitしないマイクロステップでは仕様状態がstutterし、commit時だけISAを1ステップ進めてもbindingを保つ
3. commitはISA側が実行可能なときだけ発生（HALT後の偽commitを拒否）
4. 指定されたstall条件では実装状態を保持
5. 進行が有効な非commitステップでは、状態から計算した有限のunsigned rankが減る
6. 関係・進行条件・commitが空でないことを確認

bindingとrankは **状態だけ**に依存させる。現在の入力に依存するものは拒否する。入力が次のサイクルで変わると、各サイクル内の確認だけでは帰納的な対応やrankの減少がつながらないため。

進行の主張は条件付き。「resetせず、進行条件が有効な非commitステップが続くなら無限には続かない」というもの。外部stallが永遠に続いても進む、任意のプログラムがHALTする、という保証ではない。非空性の検査も、resetからの到達可能性とは別。

## 得られた結果

- CPUの8義務が通過
- 誤ったLOAD／分岐／STORE、stall無視、commit欠落、fetch停止、HALT後commit、reset誤りを拒否
- **fetchで停止する版は状態対応の検査だけなら通り、rankによる進行の検査で拒否**
- 入力依存のbinding/rank、重複JSONキー、未知演算、型不一致を拒否
- UNKNOWN、異常なZ3出力、SATモデル取得時の再検査失敗を成功として扱わない
- 0.2時点でRustの12テストが通過（0.3でprogram契約のテストを追加）
- 別実装のISAオラクルと具体IR評価器で23試行・7,300サイクル・1,161 commitを確認。全opcode、PC wrap、自己ループ、stall、途中reset、無効なlive program入力変更を含む
- 8種類の故障から出た12個の反例を、Z3をimportしない評価器で再生

汎用性の小さな確認として、16ビットcounterを8ビットの上位／下位レジスタで実装する別モデルも同じエンジンで通した。carryを壊した版は拒否される。

## 構成

- `src/ir.rs`：型付きの共有式、メモリの局所的な正規化
- `src/frontend.rs`：型・名前・wire DAGの確認
- `src/checker.rs`：stutter/commit、reset、stall、rankの義務
- `src/solver.rs`：SMT-LIB生成、Z3プロセスとの入出力
- `src/json_input.rs`：重複キーを拒否するJSON読込み
- `examples/`：CPU・分割counterと負例
- `audit/`：独立した具体実行と反例の再生

形式は `SCHEMA.md`、言語と将来の検証方法は `VERIFICATION-ja.md`。

## 独立監査の再実行

```sh
cargo build --locked
python audit/cpu_simulation.py
Z3_BIN=/path/to/z3 python audit/replay_counterexamples.py
```

生の各証明義務とZ3モデルは結果ディレクトリの `.smt2` / `.out` に保存する。再実行で更新されるので、初回の証跡を保つ場合はコピーして使う。

## 制限

- 0.1のPython版に対する全面的な互換移植ではなく、commit/stutterに焦点を当てたversion 2のIR
- word/address幅は1〜64ビット。CPUは単一クロックの抽象モデル
- パイプライン、OOO、割込み、例外、自己書換えコード、BRAMマクロは未実装
- Z3問い合わせの制限は各check-satの10秒。全工程のwall-clock上限ではない
- 仕様が意図を十分に表すこと、Rustの義務生成・正規化、SMT変換自体の正しさは形式的には未検証
- 帰納条件の反例は、resetから到達するとは限らない。実装の誤りと、与えた関係の不足を区別する必要がある
