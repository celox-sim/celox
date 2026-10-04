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

未対応の構文は、無視せずに、その構文名を示す `Unsupported` エラーを返します。

- interface と modport、クラス、タイミング制御を持つ task。
- 振る舞い記述・検証向けの構文：`initial`、`final`、遅延、クロックエッジ以外のイベント制御、
  アサーション、`$display` などのシステムタスク、`force` / `release`。
- `always_latch`、`@*` 以外のレベルセンシティブなセンシティビティリスト、ラッチを推論する
  不完全な組み合わせ代入。
- 境界が定数でないループ（`while`、`repeat`、`forever`）。
- 実行時の値を使うべき乗。
- union、unpacked 構造体、文字列、`real`、DPI、ゲートプリミティブ、トライステートバスと
  複数ドライバ、階層参照。
- `always_ff` のイベントリストにある、4 値のクロック・リセット信号。

## 知っておくべき挙動

- **範囲外の選択。** ベクタの端を越える実行時の選択は、足りないビットを 0 として読み、
  書き込みは存在するビットだけを更新します。4 値シミュレーションでも、完全に範囲外の読み出しは、
  まだ `X` になりません。
- **ワイルドカード比較。** `casez`、`casex`、`inside`、`==?` は、定数パターンの `?`、`x`、`z`
  ビットを、2 値・4 値のどちらのシミュレーションでも尊重します。
- **package** は、使うモジュールごとにインライン展開します。名前は素の識別子で解決するため、
  package の項目とモジュールの項目が同名だと、重複宣言として報告します。
- **ブロックローカル変数**（`always_comb` 内）は、モジュールの信号になります。ほかの信号と
  名前が衝突するものは拒否します。
