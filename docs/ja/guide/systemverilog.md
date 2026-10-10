# SystemVerilog サポート

Celox は [Veryl](https://veryl-lang.org/) と、**SystemVerilog の合成可能なサブセット**をシミュレートします。
2 つのフロントエンドは同じ中間表現へ lowering するため、スケジューリング、最適化、バックエンド、
ランタイムを共有します。2 つの言語を混在させることもでき、Veryl のトップから SystemVerilog の
モジュールをインスタンス化できます。

SystemVerilog フロントエンドは、`celox` crate の Cargo feature `systemverilog` の背後にある Rust API です。
TypeScript パッケージと Vite プラグインは、現時点では Veryl だけを読み込みます。

```toml
[dependencies]
celox = { version = "*", features = ["systemverilog"] }
```

```rust
use celox::Simulator;
use std::path::Path;

let source = r#"
    module Counter(input logic clk, input logic rst, output logic [7:0] q);
        always_ff @(posedge clk) q <= rst ? 8'd0 : q + 8'd1;
    endmodule
"#;
let mut sim = Simulator::from_sv_sources(
    vec![(source, Path::new("counter.sv"))],
    "Counter",
)
.build()?;
```

`Simulator::from_mixed_sources` は、Veryl のソースと SystemVerilog の子モジュールを組み合わせます。

## サポートしているもの

同期 RTL を記述する、合成向けの SystemVerilog が対象で、デザインレベルでテストします。

| 領域 | サポート内容 |
| --- | --- |
| モジュール | ANSI ポート、`parameter` / `localparam`、型パラメータ（`parameter type`）、名前付き・位置指定のポート／パラメータ接続、階層、インスタンス配列（接続は全要素に共通、または全要素の幅に等しければ要素ごとに分割） |
| generate | `for`（`genvar`）、`if`、`case`。`**` を含む定数式 |
| 型 | `logic`、`bit`、`reg`、packed ベクタ・配列・構造体、enum（暗黙値を含む）、`typedef`、メモリとして使う unpacked 配列、符号付き・符号なし |
| package | 型、パラメータ、enum、関数。`import p::*;`、`import p::x;`、`p::x` で参照 |
| 連続論理 | `assign`、`wire w = expr;` |
| 組み合わせプロセス | `always_comb`、`always @*`、ブロックローカル変数、逐次・依存するブロッキング代入、プロセスが書き込む前の変数の読み出し（直前の値） |
| 順序プロセス | `always_ff @(posedge clk)`、`always @(posedge clk or negedge rst_n)` など。ブロッキング・ノンブロッキング代入、連結を代入先とする代入、4 値のクロック・リセット信号、複数のクロックドメインで共有する非同期リセット |
| initial ブロック | `initial` ブロックと変数宣言の初期化子（`logic [7:0] v = 8'h5a;`、interface のメンバーを含む）。初期化子は `initial` ブロックより先に適用。書き込む値が定数のもの（定数の `if` / `for`、`$readmemh` / `$readmemb` を含む）は初期状態を定める。デザインの状態を読むものやシステムタスクを呼ぶものなどそれ以外は、時間を持つ `Simulation` で時刻 0 からプロセスとして実行（`Simulator` はプロセスを実行しない） |
| 文 | `if` / `else`、`case`、`casez`、`casex`、`case ... inside`、`unique` / `priority`、`for`、`while`、`do ... while`、`repeat`、`forever`、`foreach`（反復回数が定数なら展開し、そうでなければ実行時に実行）、`break` / `continue` / `return`、即時アサーション |
| 関数 | `input` / `output` / `inout` 引数を持てる `function` と、タイミング制御のない `task`。`return`、または関数名への代入で値を返す。ローカル変数と `localparam`、部分選択や複合的な代入先への代入。呼び出しはインライン展開。定数式（パラメータ、範囲）の中の定数引数による呼び出しはエラボレーション時に評価 |
| 式 | `**` を含む算術、論理、シフト、比較、リダクション、連結・複製、`?:`、`inside`、`==?` / `!=?`、キャスト（`N'(x)`、`signed'(x)`、`T'(x)`）、`$signed` / `$unsigned` |
| 選択 | 定数・実行時のビット選択と indexed part-select（`[i]`、`[i +: W]`、`[i -: W]`）。読み書きの両方、宣言の向きによらず |
| パターン | packed 構造体・packed 配列・unpacked 配列の assignment pattern（`'{a, b}`、`'{x: a, default: 0}`、`'{n{a}}`、`T'{...}`） |
| パラメータ | 整数のパラメータと、assignment pattern で与える unpacked 配列型・packed 構造体型のパラメータ（定数テーブル） |
| システム関数 | `$bits`、`$size`、`$clog2`、`$countbits`、`$countones`、`$onehot`、`$onehot0`、`$isunknown` は式・定数式の中と文として。`$signed`、`$unsigned` は式の中と文として |
| システムタスク | `always` 系のプロセスとサブルーチンの `$display`、`$write` とその `b` / `o` / `h` 形、`$error`、`$warning`、`$info`、`$fatal`、`$finish`、`$stop`。`initial` ブロックでも使えます。`$readmemh` / `$readmemb` はそれらと `initial` ブロック。Veryl の `$assert` と `$assert_continue` |
| 状態 | 2 値・4 値シミュレーション |

上記の各構文は、ソフトウェアモデルと結果を比較するテストで確認しています。Veryl が出力する
SystemVerilog に対しても、共有の Veryl 適合性スイートを実行しています。

## サポートしていないもの

未対応の構文は、無視せずに、その構文名を示す `Unsupported` エラーを返します。エラーには
その構文を追跡する issue の番号が含まれます。専用の issue がない構文は、フロントエンドの
ロードマップ [#88](https://github.com/celox-sim/celox/issues/88) を指します。

- interface と modport、クラス、タイミング制御を持つ task。
- 振る舞い記述・検証向けの構文：タイミング制御を持つ `initial` ブロック、プロセスとして
  実行される `initial` ブロック内のノンブロッキング代入、`final`、遅延と遅延付き継続代入（[#444](https://github.com/celox-sim/celox/issues/444)）、クロックエッジ以外のイベント制御、
  並行アサーション、`force` / `release`。
- `always_latch`（[#431](https://github.com/celox-sim/celox/issues/431)）、`@*` 以外のレベルセンシティブなセンシティビティリスト、
  ラッチを推論する不完全な組み合わせ代入。
- ポートとインスタンス：non-ANSI 形式のポート宣言（[#426](https://github.com/celox-sim/celox/issues/426)）、`ref` ポート（[#427](https://github.com/celox-sim/celox/issues/427)）、
  ワイルドカード接続 `.*`（[#442](https://github.com/celox-sim/celox/issues/442)）、ゲートプリミティブ（[#457](https://github.com/celox-sim/celox/issues/457)）、`bind`。
- 宣言：interface 配列のメンバーの変数宣言の初期化子、ANSI ポートの既定値、packed union（[#440](https://github.com/celox-sim/celox/issues/440)）、0 起点の降順でない
  多次元 packed 範囲（[#438](https://github.com/celox-sim/celox/issues/438)）、ドライバを持たない内部ネット（[#460](https://github.com/celox-sim/celox/issues/460)）、別の
  プロセスの変数と同名のブロックローカル変数（[#445](https://github.com/celox-sim/celox/issues/445)）、unpacked 構造体、文字列、`real`。
- 順序回路のプロセス：イベントリストの `iff` 修飾（[#452](https://github.com/celox-sim/celox/issues/452)）と単純な信号以外のエッジ
  オペランド（[#464](https://github.com/celox-sim/celox/issues/464)）、プロセスをまたいだ同一クロック（[#443](https://github.com/celox-sim/celox/issues/443)）・同一リセット
  （[#471](https://github.com/celox-sim/celox/issues/471)）の両エッジの使用。
- 組み合わせ回路のプロセス：`always_comb` 内のノンブロッキング代入（[#453](https://github.com/celox-sim/celox/issues/453)）。
- ループ：手続き的なループの展開は 10,000 回までです。反復回数が実行時に決まる組み合わせ
  回路のループは、カウンタ形式の `for` である必要があります。loop-generate は 10,000 回を
  超えると拒否し（[#448](https://github.com/celox-sim/celox/issues/448)）、ビット演算の複合代入による genvar の更新は拒否します
  （[#455](https://github.com/celox-sim/celox/issues/455)）。
- 式：ストリーミング連結（[#447](https://github.com/celox-sim/celox/issues/447)）、パラメータ式での
  リダクション演算子（[#456](https://github.com/celox-sim/celox/issues/456)）、X/Z を含む値や 128 ビットを超える値など、単純な整数で
  ないパラメータ上書き（[#461](https://github.com/celox-sim/celox/issues/461)）。
- システムタスクとシステム関数：Celox は IEEE 1800-2023 第 20・21 章のすべての名前を
  知っています。呼び出した位置で対応していないもの（式の中の `$time`、`$fopen` など）は、
  使われていないパラメータや関数本体の中を含め、書かれた
  場所にかかわらず名前を示して報告します。システムタスクでもシステム関数でもない `$` 名、
  引数の数が誤っている呼び出しや関数の引数の省略、値として使ったタスクはエラーです。
- DPI、トライステートバスと複数ドライバ、階層参照。

## 知っておくべき挙動

- **インスタンス配列の要素。** 要素は宣言どおりの添字で参照します。`Child u[3:2](...)` の
  `u[3]` には `child_signal(&[("u", 3)], "y")` や `dut.u[3]` で届き、添字は
  `InstanceHierarchy::index` にも入ります。負の境界を持つ配列は拒否します。
- **範囲外の選択。** ベクタの端を越える実行時の選択は、足りないビットを `X`（2 値シミュレーションでは 0）
  として読み、書き込みは存在するビットだけを更新します。
- **ワイルドカード比較。** `casez`、`casex`、`inside`、`==?` は、定数パターンの `?`、`x`、`z`
  ビットを、2 値・4 値のどちらのシミュレーションでも尊重します。
- **package** はそれぞれ独立したスコープとして 1 回だけ解析し、名前は IEEE 1800-2023 26.3
  の規則で解決します。`p::x` は package `p` の項目を指すため、複数の package、または package と
  モジュールに同名の項目があっても衝突しません。`import p::x;` で見えるのは `x` だけです。
  `import p::*;` では、スコープ自身の宣言が package の同名の項目を隠し、2 つの wildcard import
  の package が宣言する名前は、参照したときだけエラーになります。generate ブロック内の import は、そのブロック
  （と内側のブロック）でだけ名前を見えるようにし、モジュール自身の宣言や import より優先されます。package の関数の中の名前は
  package の中で解決します。package の変数は、それを使うすべてのモジュール（Veryl からインスタンス化した
  SystemVerilog モジュールを含む）で共有される 1 つのオブジェクトで、初期化子も反映します。
  `sim.signal("p::v")` で参照できます。1 つの package 変数を複数のドライバが書く設計は、書き込む
  モジュールのインスタンスをすべて数えて拒否します。package 変数のクロック・リセットとしての使用、定数でない package 変数の初期化子、
  package の net は未対応です（[#1146](https://github.com/celox-sim/celox/issues/1146)）。`export` 宣言（26.6）は、import
  した宣言をその package を import する側から見えるようにします。`export p::*;` が export するのは
  package が `p` から実際に import した名前だけです。export を経由した import は元の宣言を指すため、
  2 つの経路で同じ宣言を import しても曖昧になりません。`q::x` は `q` が `x` として export する宣言も指します。
- **コンパイル単位。** ソースファイルはそれぞれ独立したコンパイル単位です（IEEE 1800-2023 3.12.1）。
  モジュールや package の外にあるパラメータ、型、サブルーチン、変数、import は、そのファイルのモジュールを
  囲むスコープ `$unit` になります。モジュールからは、自身の宣言と import の次に、それより前に宣言された
  コンパイル単位の項目と import、およびコンパイル単位のすべてのサブルーチンが見えます。`$unit::x` は
  コンパイル単位の項目 `x` を指します。コンパイル単位の変数は、そのファイルのモジュールが共有する 1 つの
  オブジェクトで、package 変数と同じく `$unit::v` という名前になります。これを宣言できるソースファイルは
  1 つだけです。コンパイル単位での DPI import は未対応です。
- **ブロックローカル変数**（`always_comb` 内）は、モジュールの信号になります。ほかの信号と
  名前が衝突するものは拒否します。
- **変数の寿命**は IEEE 1800-2023 6.21 に従います。プロセスのブロック、関数、タスクの中で宣言した
  変数は、その変数自身、そのサブルーチン、またはそのモジュール（`module automatic`）が `automatic`
  と宣言されていない限り static です。ブロックの static なローカル変数は、ブロックの実行をまたいで値を
  保持し、初期化子は時刻 0 に 1 回だけ実行します。`always_ff` 内の `int count; count++;` は数を数えます。
  automatic なローカル変数は、ブロックに入るたびに初期化します。どの経路でも読む前に書き込むため、入った
  時点の値が読まれない static なローカル変数は、どちらでも同じ動作です。static なローカル変数は値を保持する
  ため、一部の経路でしか書き込まない `always_comb` のローカル変数はラッチになり、ほかのラッチと同じく
  拒否します。呼び出しをまたいで値を保持する関数・タスクの static なローカル変数と、初期化子が定数でない
  static なローカル変数は未対応です。`automatic` と宣言してください。static な関数の結果も static なので、
  結果を代入しない経路を持つ関数（2 値の引数のすべての値を項目が網羅する `case` を除く）も拒否します。既定の寿命（`interface automatic`）が
  使用先のモジュールと異なる interface は拒否します。
- **実行時のループ。** 反復回数が実行時の値で決まる `always_ff` 内のループは、生成コード内の
  ループとして実行します。進まなくなったループは、ループ変数を示す実行時エラーを報告します。
- **文として呼び出したシステム関数**（`$countones(f(a));` など）は、式の中の同じ呼び出しと
  同様に検査・評価し、値を捨てます。`$bits` と `$size` は被演算子を評価しません。
- **定数関数**は、呼び出す表示タスクと重大度タスクを無視します。
- **`$readmemh` / `$readmemb`** は、デザインのコンパイル時にファイルを読みます。ファイルが
  ないか不正な場合は `MemoryFile` エラーです。`always_ff` では起動のたびにファイルの各ワードを
  書き込み、`initial` では初期状態の一部になります。
- **集約型の定数パラメータ**（unpacked 配列や、パターンで与える packed 構造体）は、
  シミュレーション開始時から値を保持する変数です。エラボレーション時の定数が必要な場所では
  使えません。

## Veryl が出力する SystemVerilog

Veryl の適合性スイートは、Veryl が出力する SystemVerilog に対しても実行します。対象外として
残るケースは次のとおりです。

- 位置指定の要素と `default:` を混在させた配列リテラル（`'{a, default: b}`）に対して、Veryl
  は不正な SystemVerilog を出力します。
- Celox の Veryl フロントエンドは、`always_ff` 内での出力引数や副作用を持つ関数呼び出しを
  オプトインで扱えますが、Veryl 自身はこれを拒否します。出力される SystemVerilog の呼び出し
  では、Veryl のノンブロッキングの意味ではなく即座にコピーアウトされてしまうためです。
  こうしたデザインには対応する SystemVerilog がなく、出力されません。
- テストベンチ用のモジュール（クロックや `$finish` のスケジューリングを伴う `initial`
  ブロック、階層代入）と interface は、シミュレーション可能な SystemVerilog モジュールとして
  出力されません。
