# SystemVerilog サポート

Celox は、SystemVerilog で記述した同期 RTL を **Rust API** からシミュレートできます。
Cargo feature の `systemverilog` を有効にし、ソースファイルとトップモジュールを指定して使います。
TypeScript パッケージと Vite プラグインは、現時点では Veryl のみを読み込みます。

モジュール、メモリ、interface、2 値・4 値シミュレーションなど、SystemVerilog の一部に対応しています。
バックエンドとランタイムは Veryl と共通です。個々の構文の対応状況は
[言語リファレンス](./systemverilog-reference)で確認できます。

## 最初のモジュールを動かす

Rust プロジェクトで feature を有効にします。

```toml
[dependencies]
celox = { version = "*", features = ["systemverilog"] }
```

`Simulator::from_sv_sources` にソース文字列とそのパスを渡し、トップモジュール名を指定します。

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

let clk = sim.event("clk");
let rst = sim.signal("rst");
let q = sim.signal("q");
sim.modify(|io| io.set(rst, 1u8))?;
sim.tick(clk)?; // カウンタをリセット。
sim.modify(|io| io.set(rst, 0u8))?;
sim.tick(clk)?; // 立ち上がりエッジを1回進める。
assert_eq!(sim.get(q), 1u8.into());
```

ファイルが複数ある場合は、同じリストに追加します。各ファイルは独立したコンパイル単位です。
トップには SystemVerilog のモジュール名を指定します。

## デザインの動かし方を選ぶ

| テストで行いたいこと | API |
| --- | --- |
| Rust から入力を設定し、論理を評価してクロックイベントを進める | `Simulator::from_sv_sources` |
| シミュレーション時刻を進め、タイミング制御を持つ `initial` / `always` プロセスを実行する | `Simulation::from_sv_sources` |
| Veryl と SystemVerilog のソースを一緒に扱う | `Simulator::from_mixed_sources` |

`Simulator::tick` は指定イベントの立ち上がりエッジを駆動します。
`Simulation` は時刻を管理し、スケジュールされたエッジを実行します。
定数による初期化はどちらにも適用されます。デザインの状態を読む、またはシステムタスクを呼ぶ
`initial` ブロック、タイミング制御を持つプロセス、`@*` でも
[文が読む変数](./systemverilog-reference#timing-controls)でもないセンシティビティリストを持つ
`always` はプロセスとして実行するため `Simulation` が必要です。`Simulator` はプロセスを実行しないため、
このようなデザインでは `SimulatorErrorKind::ProcessRequiresSimulation` でビルドに失敗し、
最初のプロセスとそのソース位置を示します。Veryl と混在する階層では
`Simulation::from_sources(veryl).with_sv_sources(sv)` で `Simulation` を作成します。

X/Z を扱う場合は builder に `.four_state(true)` を指定します。
状態モデルは [4 値シミュレーション](./four-state)、時刻、`$stop`、メモリの読み込みなどは
[実行時のリファレンス](./systemverilog-reference#semantics-worth-knowing)で説明しています。

## サポートしているもの

主な対応範囲は次のとおりです。

| 領域 | 例 |
| --- | --- |
| デザイン構造 | モジュール階層、パラメータ、generate、package、interface と modport |
| データ | packed ベクタ、多次元 packed 配列、構造体、enum、unpacked メモリ |
| RTL のプロセス | `assign`、`always_comb`、`always @*`、ブロッキング代入の `always_latch`、エッジで起動する `always` と `always_ff` |
| 時間を持つプロセス | `#delay`（`#5ns` などの時間リテラルを含む）、変数・エッジ・選択・式・`@*` に対する `@(...)`、`wait (...)`、ノンブロッキング代入、タイミング制御を持つ task 呼び出しを使う `initial` と `always` |
| 演算と制御 | 算術、部分選択、assignment pattern、分岐、ループ、関数 |
| シミュレーション補助 | 即時アサーションと遅延付き即時アサーション、表示タスク、`$readmemh` / `$readmemb`、時刻の取得、乱数 |

各領域には制約があります。デザインを移植する際は、
[詳しい対応一覧](./systemverilog-reference#what-is-supported)でポート、型、文、システム関数を確認してください。

## サポートしていないもの

デザインの変更が必要になりやすいのは、次の機能です。

- [単純なクラス](./systemverilog-reference#classes)を超えるクラスを使う検証、[単一サイクルBoolean部分集合](./systemverilog-reference#並行アサーション)を超える並行アサーション。
- `fork` / `join`、named event、`a = #1 b` のような代入内のタイミング制御、
  `always_ff` のイベント制御の `iff`（[#452](https://github.com/celox-sim/celox/issues/452)）。
- ラッチ内のノンブロッキング代入、複数ドライバ、トライステートバス。`always_comb` の暗黙のラッチ推論は 共通の `[diagnostics]` 設定で `inferred_latch = "warn"` または `"allow"` を指定して許可します。
- 非 ANSI モジュールポート、ワイルドカード `.*` 接続、ゲートプリミティブ、`bind`。
- `real`、ポート・固定長配列の unpacked 構造体、interface の一部の機能。

[制約の一覧](./systemverilog-reference#what-is-not-supported)に、より細かな条件と追跡 issue をまとめています。
未対応の構文は、構文名と追跡 issue を含む診断を返します。

## Veryl と混在させる

どちらの言語もトップにでき、モジュール階層は双方向に言語境界を跨げます。
`Simulator::from_mixed_sources` に両言語のソースを渡します。
Veryl からは `$sv::Module`、SV からは `prj_Module` などの Veryl の出力名で参照します。

interface と `input` / `output` modport も言語間で接続できます。
名前の付け方、interface パラメータ、言語間の制約は
[Veryl と SystemVerilog の混在](./systemverilog-reference#mixed-language)を参照してください。

## DPI-C imports

`sv-dpi` を有効にすると、対応する C 関数を SV のクロックで起動するプロセスから呼び出せます。
関数や共有ライブラリは `dpi_function` / `dpi_library` で登録します。
C と SV の型を一致させる必要があるため、どちらも `unsafe` API です。
対応する型と呼び出し位置は [DPI-C imports](./systemverilog-reference#dpi-c-imports)を参照してください。

## 個別の挙動を調べる {#知っておくべき挙動}

- [初期化、スコープ、変数の寿命、メモリファイル](./systemverilog-reference#semantics-worth-knowing)
- [プロセス内のイベント待ちとレベル待ち](./systemverilog-reference#timing-semantics)
- [時間単位、精度、時刻の取得](./systemverilog-reference#time-and-precision)
- [乱数のシードと再現性](./systemverilog-reference#random-numbers)

## Veryl が出力する SystemVerilog

Veryl の適合性スイートは、Veryl が出力した SystemVerilog も検証します。
出力できないケースや言語間で意味が異なるケースは、[残る対象外のケース](./systemverilog-reference#systemverilog-emitted-by-veryl)を参照してください。
