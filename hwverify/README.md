# hwverify-rs 0.13.0

Rustによる、ハードウェア向け状態対応チェッカー。小さなCPUを題材に、**複数命令が同時に進むin-order pipelineのretireを、ISAの1ステップへ対応づける**ところまで実装した。既定はZ3 fallback、有限Bool/BVにはZ3を呼ばない選択経路もある。

中核の型付きIR・正規化・証明義務・有限Bool/BVソルバーはRust実装。既存のSMT-LIB/Z3経路も選択肢として残る。Veryl conformanceアダプターではPythonでSIRを式へ変換し、RustサービスでSAT/UNSATを判定する。Pythonは実例生成・移行・独立監査にも使う。

現状は **Rustの構造的UNSATカーネル、選択した有限Bool/BV solverまたはZ3 fallbackの結果、Rust側の変換を信頼する**。処理系自身に形式的な正しさの証明を付けたものではない。

## 0.13.0: 元のRustテスト665件をproof-backed Backendで実行

元の `celox-test-suite-veryl` のテスト本体・期待値を変更せず、Backendの読み出しを
自前の有限Bool/BVソルバーで実行する経路を追加。各読み出しの前に実行可能性をSATで、
返す値の一意性をUNSATで確認する。読み出した値を次の入力や関係検査に使う元テストも動く。
この経路ではZ3を呼ばない。

**全665件を実行して663件通過**。内訳は観測値を検査する652件、smoke-onlyの3件、
実際にコンパイル拒否を確認する8件。負の定数をforの範囲に使う正例2件は、
Verylの言語制約で引き続き失敗する。これを成功した反例テストへ読み替えない。
CIは「663件通過＋その2件の厳密な既知制約」というcoverage contractを確認し、
新たな失敗・UNKNOWN・ケース消失・期待値や入力手順の変更を拒否する。

階層・関数・配列・複数clock・4状態値・広幅演算を対象コーパスで検査。
依存コンパイラの修正は6個の明示的なpatchとして固定し、元テストのハッシュを維持する。
0.12の独立した46件の変換経路もCIに残している。

これは与えられた有限テストの検査であり、任意の入力列・全Veryl機能への適合や
処理系自身の正しさを証明したものではない。UNSATは自前ソルバーを信頼し、
保存したqueryは再実行用の記録であって独立した証明書ではない。
[再現手順・対応範囲・既知制約](conformance/veryl-proof/README.md)、
[独立した追加検査](audit/veryl_proof_independent/README.md)を参照。

## 0.12.0: Verylの実テストをZ3なしのFVとしてCIで実行

Celoxの固定revisionから実際のRust stimulus/期待値を自動抽出し、Veryl 0.21.0の
parser/analyzerを経由して有限traceを検証するCIを追加。**665ケースを全件分類、
556ケース・6,142 assertionを抽出し、対応範囲の46ケース・118フレーム・104 assertionを検証**する。
期待値を回路から作らず、実行可能性と各assertionの反例不存在を別々に検査する。
全対応ケースの負例・元ソース改変対照・実行可能なZ3 tripwireもCIで必ず走る。

未対応とUNKNOWNは成功にしない。固定coverage manifestにより、ケース消失・skip化・
assertion減少・ソース変化を検出する。scalar read、unary/reduction、ternary、if、比較、
cast、shift等を拡張したが、一般のVeryl全体・4状態・非同期resetの完全対応ではない。
[再現コマンド・対応範囲・coverage gate](conformance/veryl/README.md)を参照。

## 0.11.3: 呼び出し側の予想に合わせた探索

呼び出し側が宣言したSAT/UNSATの予想を、**探索順序だけのhint**として使う。
通常の証明義務はUNSAT向けの0.11.2型分割を使い、反例優先probeの費用を払わない。
非空性・存在確認はSAT向けのprobe＋再開可能な分割探索を使う。
`--finite-search-hint query|sat|unsat` で、論理上の合否条件を変えずに探索方針を指定できる。
例えばバグ探索は `--finite-search-hint sat`。既定の `query` は各義務の合否期待に従う。

全46モデルを各hintと2つの旧実装で5回ずつ交互測定した1,150実行で、結果と元SMTは不変。
正しい32bitの通常証明は中央値0.735秒（0.11.2は0.773秒、一律probeは0.889秒）に戻る。
SAT hintでの32bit forwarding/interlock反例は0.094/0.078秒。逆のhintでも結果は正しい。

hintは結果・仮定ではない。予想と逆のSAT/UNSATも通常どおり返し、SATは元の式とcontextを再評価する。
未対応・予算不足はUNKNOWNのまま。正しいhintなら必ず速い、予想外の結果だけが遅いという保証はない。
モデル・元SMT・共有予算は変更せず、量化例の有限solver未対応も維持する。
[API・CLI・hint×実結果の比較](audit/expected_result_search/README.md)と
[独立監査](audit/expected_result_search_independent/README.md)を参照。
以前の未公開0.11.3で試した一律反例優先方針と測定は、
[不採用方針の履歴](audit/counterexample_search/README.md)として保存している。

## 0.11.2: 32bitも同じ予算で証明

有限solverが一般のBool式の選択肢を完全に分割し、**分岐付き4/8/16/32bitの全8義務**を通す。
元の式・モデル・bindingを保持し、時間・work・clauseをquery全体で共有する。各branchへの予算増配はしない。
全ての主負例も反例のまま。ただし後の選択肢に反例がある一部のSAT queryは遅くなる。
[分割の意味・測定・残る信頼境界](BRANCH-SOLVER-DECOMPOSITION-ja.md)を参照。

## 0.11.1: 幅を増やしたときの探索量を削減

有限solver内で、必須の同値変数のbit共有とmuxの直接CNF化を行った。
モデル・ISA・binding・元SMT・既定予算は変えず、16bitの分岐付きpipelineも全8義務が通る。
32bitのforward/interlock負例も反例を取得できるようになった。0.11.1時点では32bit正例のrefinementは
既定work予算でUNKNOWNであり、未証明として扱った。
詳細・比較・信頼境界は[幅スケーリング改善](BRANCH-SOLVER-SCALING-ja.md)、
[再現可能な性能監査](audit/branch_solver_scaling/README.md)を参照。

## 0.11: 分岐・flushと幅別の検証

独立した分岐付きD/X/Wパイプライン例を追加した。Xでゼロ分岐を判定し、
誤経路の若い命令を破棄しながら、古いW命令は順序通りretireする。
0.11.0時点では4/8bitは有限solverで全8義務を確認。16/32bitは既定のwork予算でrefinementがUNKNOWNとなり、
未証明として報告する。既存の分岐なしpipelineとsolver本体は変更していない。
詳細と負例・測定結果は[分岐付きパイプライン](BRANCH-PIPELINE-ja.md)を参照。

## 0.10.1: 同じ検証義務を高速化

有限solverの変数選択をheap化し、時刻確認とwatch-listの割当てを減らした。
同じマシンで各7回を交互測定したpipeline全体の中央値は **1.814秒→0.147秒（12.31倍）**。
既定Z3経路は0.120秒。5負例も反例のまま14.55〜20.56倍速くなった。
モデル・binding・元のSMT・探索のdecision/conflict数・SAT witnessは同一で、finite内のZ3呼出しは0。
未対応・予算不足はUNKNOWNのまま。一般のCPUや任意の式に対する速度保証ではない。
[測定方法と生証跡](audit/pipeline_solver_speed/README.md)、
[独立正当性監査](audit/finite_speed_independent/README.md)を参照。

## 0.10の追加: 本当のpipelineとZ3なしの小さな検証経路

- D/X/Wの3段で最大3命令を同時に保持。W retirement、X優先のforwarding、LOAD-use interlock、全段stallをモデル化
- 2本の4bit register、4命令の任意循環ROM、任意readonly data4word。MOVI/ADDI/XORI/LOADを1命令ISAへ帰納的に対応づける
- `HWVERIFY_SOLVER=finite` はscalar Bool/BVをbit-blastして自作CDCL solverで解く。SATも元の式で再評価し、非空性と反例をZ3なしで扱う
- 元のSMT義務、finite diagnostics、SAT assignments/contextを保存。未対応のmemory/quantifierや資源不足を成功にせずUNKNOWNにする
- 通常のZ3 fallback経路と既存の量化例を維持。新しいpipeline例がZ3なしで通ることと、処理系全体のZ3完全撤去は区別する

範囲、invariant、solverの信頼境界、独立simulationと5負例は[パイプライン検証](PIPELINE-ja.md)を参照。

```sh
HWVERIFY_SOLVER=finite cargo run --release --locked -- examples/pipeline.hwv \
  --out results/new-pipeline --z3 /definitely/absent/z3
```

## 0.9.1の整理: 量化した名前をそのまま参照

`forall a: bv<4>;` で宣言した変数は `a` と書く。実例と移行printerも
`add(amount: a) => count == a;` を使う。曖昧な出力名との衝突はエラーにし、
printerは必要な場合だけ束縛名を安全に改名する。量化順序・型・検証の意味は変えない。

## 0.9の追加: 期待する関係と量化を明示したtrace

- scoped specでは `value` は現在値、`value'` は次状態。出力も同じ規則で、入力にはprimeを付けない
- `operation add { expect value' == value + amount; }` で関係を記述。名前付き `expectation` と入れ子の `all` / `any` でAND/ORを合成
- `forall a: bv<4>; execution forall;` のように入力と有限実行を別々に量化。入力prefixの順序を維持し、実行不能なforallを成功にしない
- `add(amount: a) => count == a;` で一回の操作の入力と、操作後の検査条件を並べる。期待出力を操作の前提に混ぜない
- 旧prefix・操作関係・positive/negative trace・JSON version 2/3/4は互換性を維持。新しい量化情報はversion 3/4の任意フィールドとして扱う

構文・量化順序・可実行性・有限長の制限は [期待と量化trace](EXPECTATIONS-AND-TRACES.md) を参照。
[quantified_counter.hwv](examples/quantified_counter.hwv) は全入力の加算と入力ごとの補正値、
[expectation_counter.hwv](examples/expectation_counter.hwv) は名前付きAND/OR関係の実例。
入力量化prefixの後に一つの実行量化を置く範囲であり、任意の量化交互配置やlivenessではない。

## 0.8の追加: 直接宣言、局所ポート、独立操作

- `design "名前"` / `specification "名前"` ヘッダの後に宣言を並べる。文書全体の波括弧とヘッダ末尾のセミコロンは不要
- version 2/3の互換表記も `input amount: bv<4>;`、`observation count: bv<4>;`、`state value: bv<4>;`、`parameter limit: bv<4>;` で個別に型を宣言
- version 3では `operation add {}`、`component Counter { ... }`、`composition Budgeted { ... }` をルートに、`example overspend { ... }` を部品・合成に直接置く
- version 3のbindingでは `bind Counter { value = s.count; }` と `observation count = s.count;` で写像を指定
- 旧コンテナ・外側の波括弧は互換表記として受理。既存JSON schema version 2 / 3と検証の意味を維持
- 新しいversion 4では `spec Counter(input amount: bv<4>, output count: bv<4>)` に局所ポートを宣言し、`use left: Counter(amount: a, count: x);` で明示配線。繰り返し利用と入れ子でも非公開状態を分離
- `operation left = actions(left.add);` で子操作を公開。traceの `actions(left, right)` は同時実行、`actions()` は全て非選択。非選択の葉の非公開状態は保持するが、公開出力を一律には固定しない
- 実装の独立したselectorは同時にtrueでよい。同じ葉の異なる操作だけに排他性を証明し、selectorの全ての部分集合を列挙しない

空の宣言群はDSLで省略でき、対応するJSON mapには `{}` を補う。
意味検査は従来通りで、操作・部品の非空要件や必須の代入・述語ブロックは変わらない。
既存version 2/3の `reset` / `next` / `steps`、実装の操作selector `operations`、trace内の操作・値指定も従来通り。
旧形式の移行方法と正確な省略範囲は [LANGUAGE.md](LANGUAGE.md) と
[SPECIFICATION-LANGUAGE.md](SPECIFICATION-LANGUAGE.md) を参照。
新しい局所ポート・操作集合の意味は [SCOPED-SPECIFICATION.md](SCOPED-SPECIFICATION.md)、
片方ずつ・両方同時・両方停止の実例は [scoped_dual_operator.hwv](examples/scoped_dual_operator.hwv) を参照。
操作は論理的な関係であり、RTLの `always_ff` やクロックイベントではない。

## 0.7の追加: 合成仕様と正例・負例

- `specification`文書で、共有する観測・入力と、各部品の非公開状態を分離
- 複数の関係仕様を同じ名前付き操作で同期し、条件の論理積として合成。合成の入れ子にも対応
- 部品・合成仕様に有限トレースの正例（SAT）・負例（UNSAT）を添付。省略した観測・入力・内部状態は存在量化し、0等で補わない
- 正例のSATにも完全な状態・観測・入力のwitnessを保存。UNKNOWNやwitness再確認の失敗を成功にしない
- 任意の状態専用bindingを与え、実装のreset・操作・stutterが合成関係仕様に従うことを別途検証する限定的bridge

[合成仕様の言語・意味論](SPECIFICATION-LANGUAGE.md)を参照。
[budgeted_counter.hwv](examples/budgeted_counter.hwv)は算術仕様と予算仕様の合成とbindingが通る例。
[contradictory_composition.hwv](examples/contradictory_composition.hwv)は単体で満たせる2仕様が合成後の正例を拒否する例、
[weakened_budget.hwv](examples/weakened_budget.hwv)は弱すぎる仕様が負例を受け入れる例で、後者2つは意図的に失敗する。

**有限例が通っても、仕様の完全性・普遍的正しさ・deadlock freedom・実装の進行を証明したことにはならない。**
bridgeは与えた決定的抽象化によるsafety/stuttering refinementであり、永遠にstutterする実装も通り得る。
正例を実装が実現できるという主張もしない。旧version 2の単一仕様・進行・全正当性検査は従来通り別に利用できる。

```sh
cargo run --locked -- examples/budgeted_counter.hwv --out /tmp/budgeted --z3 /path/to/z3
cargo run --locked -- examples/contradictory_composition.hwv --out /tmp/contradiction --z3 /path/to/z3 # exit 1
cargo run --locked -- examples/weakened_budget.hwv --out /tmp/weak --z3 /path/to/z3 # exit 1
```

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
- `examples/*.hwv`：配列和・memory increment・関係仕様の言語版。既存の式木・trace順序を保持した個別宣言表記
- `scripts/json_to_hwv.py`：v2/v3/v4 JSONをschema versionを保ったまま個別宣言表記にするprinter
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
- pipelineは上記の小さなD/X/Wモデルのみ。OoO、分岐pipeline、割込み、例外、自己書換えコード、BRAMマクロは未実装
- Z3は通常各check-sat10秒、自動保存partitionは1秒。保存loopは30秒で次の問い合わせを停止し、未実行をUNKNOWNにする。試行選択・カーネル・既に走るquery・証跡I/Oを含めたhard wall-clock上限ではない
- 仕様が意図を十分に表すこと、Rustの義務生成・正規化、SMT変換自体の正しさは形式的には未検証
- 帰納条件の反例は、resetから到達するとは限らない。実装の誤りと、与えた関係の不足を区別する必要がある
