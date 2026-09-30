# hwverify-rs 0.6

Rust＋Z3による、ハードウェア向け状態対応チェッカー。小さなCPUを題材に、**実装の複数マイクロサイクルを、ISAの1ステップへ対応づける**ところまで実装した。

この版はRustで型付きIR・正規化・証明義務を構築し、小さなカーネルで閉じない義務のSMT-LIBをZ3へ送る。Pythonチェッカーを呼ぶラッパーではない。Pythonファイルは実例の生成と独立監査にだけ使う。

現状は **Rustの構造的UNSATカーネル、Z3 fallbackのUNSAT結果、Rust側の変換を信頼する**。処理系自身に形式的な正しさの証明を付けたものではない。

## 0.6の追加

- Cargo workspaceを`ir` / `solver` / `verify` / `syntax` / `cli`へ分離。solverはparolに依存しない
- parolで生成したparserによる`.hwv`言語。state、reset/next、binding、progress、pre/invariant/postを直接記述する
- parserのASTと型付きIRを分離し、全宣言・未使用wire・契約を検証してからsolverを起動する
- filename・行・列・source span付きの構文／名前／型エラー。生成されたASTも検証を省略できない
- 既存のJSON形式・CLI・証明義務を維持。`--check`と`--emit-json`はsolverなしで検証／移行確認ができる

言語は[LANGUAGE.md](LANGUAGE.md)、crate境界・再現手順は[WORKSPACE.md](WORKSPACE.md)。既存の結果・監査archiveは過去版の証跡として変更しない。

## 0.5の追加

- 候補を実際に代入・簡約し、case増加に対する限界利益で自動分割を選ぶ
- Bool/BV/memoryの小さなUNSAT-onlyカーネル。閉じない義務は元の式をZ3へ渡す
- 元のSMT義務、backend、カーネル/選択時間を保存し、外部再検査ができる
- SAT/UNKNOWNは推測しない。Leanで新規則が認証済みという主張もしない

方法、制限、成功と回帰の全実測は[構造的solver](STRUCTURAL-SOLVER-ja.md)。
最初の版は配列和で遅くなったが、hash再計算を除いた最終版では同条件の0.4より高速。
失敗した初期実装の実測も保存しており、一般的な速度改善の保証はしない。

## 0.4の追加

- 手動splitなしで、invariant/modelの比較定数から完全な保存partitionを自動選択
- 候補・採否・予算をreportに記録し、未実行/UNKNOWNを成功にしない
- 同じ配列和、rename/式の並べ替え、別プログラムと対抗例で検査
- 最初の選択戦略のUNKNOWNも保存。manualより高速・万能という主張ではない

選択規則・全実測・制限は[自動分割](PARTITIONING-ja.md)。

## 0.3時点の追加

- ISA上で固定16要素のmod256配列和を帰納的に検証し、同じCPUのfetch/execute refinementへ接続
- 再利用可能な`program_contract`でpre/invariant/post/terminationと停止後のquiescenceを検査
- compact split hintsからRustが完全な場合分けを生成。ユーザーのlemma/simp指定は不要だが、invariant/rank/hintsの選択はまだ手動
- 旧CPUのopcodeは変更せず、別例でindexレジスタとADDX/IXJを追加
- 単一SMT義務の10秒timeoutも保存。成功例だけを示した結果ではない

詳細は[プログラム検証](PROGRAM-ja.md)、[メモリ正規化の形式化](proof/README-ja.md)。

## 実行

Rust/CargoとZ3実行ファイルを用意する。今回の検証環境はRust 1.98.1、Z3 5.1.0。

```sh
cargo test --workspace --locked
cargo run --locked -- examples/cpu.json --out results/new-cpu --z3 /path/to/z3
cargo run --locked -- examples/array_sum.hwv --out results/new-array-sum --z3 /path/to/z3
cargo run --locked -- examples/memory_increment.hwv --check
cargo run --locked -- examples/array_sum.hwv --emit-json /tmp/array-sum.json
```

`Z3_BIN`でもZ3の場所を指定できる。Cargo依存はlock済み。処理系の入力は`.hwv`言語または従来の宣言的JSONで、仕様・実装・binding・commit・進行条件を与える。利用者に補題名や書換え順序を要求しない。

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

- `crates/ir`：型付きの共有式、状態・遷移・契約、全入力に共通する意味検証
- `crates/solver`：構造的UNSAT、partitionの試行選択、SMT-LIB保存とZ3
- `crates/verify`：stutter/commit、reset、stall、rank、program契約の義務
- `crates/syntax`：parol文法／生成AST、source spanと診断、JSONへの互換lowering
- `crates/cli`：従来の`hwverify-rs`実行ファイルとCLI回帰テスト
- `examples/*.hwv`：配列和・memory incrementの言語版。対応JSONから式木を保持して移行
- `scripts/json_to_hwv.py`：既存v2 JSONの移行printer
- `audit/`：独立した具体実行・式評価・反例の再生

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
- Z3は通常各check-sat10秒、自動保存partitionは1秒。保存loopは30秒で次の問い合わせを停止し、未実行をUNKNOWNにする。試行選択・カーネル・既に走るquery・証跡I/Oを含めたhard wall-clock上限ではない
- 仕様が意図を十分に表すこと、Rustの義務生成・正規化、SMT変換自体の正しさは形式的には未検証
- 帰納条件の反例は、resetから到達するとは限らない。実装の誤りと、与えた関係の不足を区別する必要がある
