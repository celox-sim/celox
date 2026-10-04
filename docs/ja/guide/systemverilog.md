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
| 組み合わせプロセス | `always_comb`、`always @*`、ブロックローカル変数、逐次・依存するブロッキング代入 |
| 順序プロセス | `always_ff @(posedge clk)`、`always @(posedge clk or negedge rst_n)` など |
| 文 | `if` / `else`、`case`、`casez`、`casex`、`unique` / `priority`、定数境界の `for`、展開されたループ内の `break` / `continue` |
| 関数 | `input` / `output` / `inout` 引数を持てる `function` と、タイミング制御のない `task`。`return`、または関数名への代入で値を返す。呼び出しはインライン展開 |
| 式 | 算術、論理、シフト、比較、リダクション、連結・複製、`?:`、`inside`、`==?` / `!=?`、キャスト（`N'(x)`、`signed'(x)`、`T'(x)`）、`$signed` / `$unsigned` |
| 選択 | 定数・実行時のビット選択と indexed part-select（`[i]`、`[i +: W]`、`[i -: W]`）。読み書きの両方、宣言の向きによらず |
| パターン | packed 構造体の assignment pattern（`'{a, b}`、`'{x: a, default: 0}`） |
| システム関数 | `$bits`、`$size`、`$clog2`（定数引数）、`$countones`、`$onehot`、`$onehot0`、`$isunknown` |
| 状態 | 2 値・4 値シミュレーション |

上記の各構文は、ソフトウェアモデルと結果を比較するテストで確認しています。Veryl が出力する
SystemVerilog に対しても、共有の Veryl 適合性スイートを実行しています。

## サポートしていないもの

未対応の構文は、無視せずに、その構文名を示す `Unsupported` エラーを返します。エラーには
その構文を追跡する issue の番号が含まれます。専用の issue がない構文は、フロントエンドの
ロードマップ [#88](https://github.com/celox-sim/celox/issues/88) を指します。

- interface と modport、クラス、タイミング制御を持つ task。
- 振る舞い記述・検証向けの構文：`initial`（[#425](https://github.com/celox-sim/celox/issues/425)）、`final`、遅延と遅延付き継続代入
  （[#444](https://github.com/celox-sim/celox/issues/444)）、クロックエッジ以外のイベント制御、アサーション、`$display` などの
  システムタスク、`force` / `release`。
- `always_latch`（[#431](https://github.com/celox-sim/celox/issues/431)）、`@*` 以外のレベルセンシティブなセンシティビティリスト、
  ラッチを推論する不完全な組み合わせ代入。
- ポートとインスタンス：non-ANSI 形式のポート宣言（[#426](https://github.com/celox-sim/celox/issues/426)）、`ref` ポート（[#427](https://github.com/celox-sim/celox/issues/427)）、
  ワイルドカード接続 `.*`（[#442](https://github.com/celox-sim/celox/issues/442)）、ゲートプリミティブ（[#457](https://github.com/celox-sim/celox/issues/457)）、`bind`。
- 宣言：変数宣言の初期化子（[#439](https://github.com/celox-sim/celox/issues/439)）、packed union（[#440](https://github.com/celox-sim/celox/issues/440)）、0 起点の降順でない
  多次元 packed 範囲（[#438](https://github.com/celox-sim/celox/issues/438)）、ドライバを持たない内部ネット（[#460](https://github.com/celox-sim/celox/issues/460)）、別の
  プロセスの変数と同名のブロックローカル変数（[#445](https://github.com/celox-sim/celox/issues/445)）、unpacked 構造体、文字列、`real`。
- 順序回路のプロセス：`always_ff` 内のブロッキング代入（[#421](https://github.com/celox-sim/celox/issues/421)）、連結を代入先とする
  代入（[#450](https://github.com/celox-sim/celox/issues/450)）、イベントリストの `iff` 修飾（[#452](https://github.com/celox-sim/celox/issues/452)）と単純な信号以外のエッジ
  オペランド（[#464](https://github.com/celox-sim/celox/issues/464)）、プロセスをまたいだ同一クロック（[#443](https://github.com/celox-sim/celox/issues/443)）・同一リセット
  （[#471](https://github.com/celox-sim/celox/issues/471)）の両エッジの使用、イベントリストにある 4 値のクロック・リセット信号。
- 組み合わせ回路のプロセス：`always_comb` 内のノンブロッキング代入（[#453](https://github.com/celox-sim/celox/issues/453)）。
- ループ：展開できるのは `for` だけです。`foreach`、`while`、`do-while`、`repeat`、
  `forever` は反復回数が定数でも拒否します（[#441](https://github.com/celox-sim/celox/issues/441)、[#459](https://github.com/celox-sim/celox/issues/459)）。loop-generate は
  10,000 回を超えると拒否し（[#448](https://github.com/celox-sim/celox/issues/448)）、ビット演算の複合代入による genvar の更新は拒否します
  （[#455](https://github.com/celox-sim/celox/issues/455)）。
- 式：定数式以外での `**`（[#470](https://github.com/celox-sim/celox/issues/470)）、ストリーミング連結（[#447](https://github.com/celox-sim/celox/issues/447)）、パラメータ式での
  リダクション演算子（[#456](https://github.com/celox-sim/celox/issues/456)）、X/Z を含む値や 128 ビットを超える値など、単純な整数で
  ないパラメータ上書き（[#461](https://github.com/celox-sim/celox/issues/461)）。
- 関数：`v[0] = 1'b1` のような、部分選択や複合的な代入先への代入（[#466](https://github.com/celox-sim/celox/issues/466)）。
- DPI、トライステートバスと複数ドライバ、階層参照。

## 知っておくべき挙動

- **インスタンス配列の要素。** 要素は宣言どおりの添字で参照します。`Child u[3:2](...)` の
  `u[3]` には `child_signal(&[("u", 3)], "y")` や `dut.u[3]` で届き、添字は
  `InstanceHierarchy::index` にも入ります。負の境界を持つ配列は拒否します。
- **範囲外の選択。** ベクタの端を越える実行時の選択は、足りないビットを 0 として読み、
  書き込みは存在するビットだけを更新します。4 値シミュレーションでも、完全に範囲外の読み出しは、
  まだ `X` になりません。
- **ワイルドカード比較。** `casez`、`casex`、`inside`、`==?` は、定数パターンの `?`、`x`、`z`
  ビットを、2 値・4 値のどちらのシミュレーションでも尊重します。
- **package** は、使うモジュールごとにインライン展開します。名前は素の識別子で解決するため、
  package の項目とモジュールの項目が同名だと、重複宣言として報告します。
- **ブロックローカル変数**（`always_comb` 内）は、モジュールの信号になります。ほかの信号と
  名前が衝突するものは拒否します。
