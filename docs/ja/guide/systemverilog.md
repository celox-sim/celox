# SystemVerilog サポート

Celox は **Rust API** から SystemVerilog をシミュレートできます。
ソースファイルとトップモジュールを指定して使います。
TypeScript パッケージと Vite プラグインは、現時点では Veryl のみを読み込みます。

Celox は IEEE Std 1800™-2023 に従います。
標準をなぞる代わりに、[逸脱事項のページ](./systemverilog-reference)では
未対応の構文、意図的な逸脱、処理系定義の選択だけを挙げています。
そこに無い挙動は標準どおりです。見つからない差異は不具合として扱ってください。

## 最初のモジュールを動かす

Rust プロジェクトに Celox を追加します。

```toml
[dependencies]
celox = "*"
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
| シミュレーション時刻を進め、時間を持つプロセスと `final` 手続きを実行する | `Simulation::from_sv_sources` |
| Veryl と SystemVerilog のソースを一緒に扱う | `Simulator::from_mixed_sources` |

`Simulator::tick` は指定イベントの立ち上がりエッジを駆動します。
`Simulation` は時刻を管理し、スケジュールされたエッジを実行します。
`Simulator` はプロセスを実行しません。そのため、タイミング制御を持つプロセス、
定数でない `initial` ブロック、`final` 手続きを持つデザインには `Simulation` が必要です。
`Simulator` で構築すると `SimulatorErrorKind::ProcessRequiresSimulation` で失敗します。
どのプロセスに `Simulation` が必要かは
[実行モデル](./systemverilog-reference#celox-execution-model)を参照してください。

X/Z を扱う場合は builder に `.four_state(true)` を指定します。
状態モデルは [4 値シミュレーション](./four-state)を参照してください。
Veryl と混在する階層では `Simulation::from_sources(veryl).with_sv_sources(sv)` を使います。

## 標準からの逸脱

未対応の構文は、構文名と追跡 issue を含む `Unsupported` エラーで拒否します。
黙って無視することはありません。
デザインを移植するときに変更が必要になりやすいのは次の領域です。

- Boolean と固定遅延 sequence の部分集合を超える並行アサーション、
  スカラ部分集合を超えるクラスのランダム化と制約。
- `bind`、checker、covergroup、clocking ブロック、`specify` ブロック、UDP、
  `defparam`、手続き的な `force` / `release`、`disable fork` 以外の `disable`。
- `always_comb` と `always_latch` 内のノンブロッキング代入、複数モジュールに
  またがるネットのドライバ（モジュール間のトライステートバス）、`always_ff` の `iff`。
- `ref` ポート、non-ANSI ポート式、遅延・強度付きゲートプリミティブ、
  interface の一部の機能。
- `always_comb` の推論ラッチ。`inferred_latch = "warn"` または `"allow"` を
  設定しない限りエラーになります。

完全な一覧は[逸脱事項のページ](./systemverilog-reference#what-is-not-supported)です。
意図的な逸脱や、乱数・時刻の既定値・
イベント実行順序といった処理系定義の選択もそこにまとめています。

## Veryl と混在させる

どちらの言語もトップにでき、モジュール階層は双方向に言語境界を跨げます。
`Simulator::from_mixed_sources` に両言語のソースを渡します。
Veryl からは `$sv::Module`、SV からは `prj_Module` などの Veryl の出力名で参照します。
interface と `input` / `output` modport も言語間で接続できます。
[Veryl と SystemVerilog の混在](./systemverilog-reference#mixed-language)を参照してください。

## DPI-C imports

手続きコードから C 関数を呼び出せます。
関数や共有ライブラリは `dpi_function` / `dpi_library` で登録します。
C と SV の型を一致させる必要があるため、どちらも `unsafe` API です。
`celox run` コマンドは `--dpi-lib PATH` で共有ライブラリをリンクします。
対応する型と呼び出し位置は [DPI-C imports](./systemverilog-reference#dpi-c-imports)を参照してください。

## 個別の挙動を調べる {#semantics-worth-knowing}

- [未対応の構文](./systemverilog-reference#what-is-not-supported)
- [意図的な逸脱](./systemverilog-reference#intentional-deviations)
- [イベント実行順序の選択](./systemverilog-reference#timing-semantics)
- [時間単位と精度の既定値](./systemverilog-reference#time-and-precision)
- [乱数のシードと再現性](./systemverilog-reference#random-numbers)
- [終了処理と `final` 手続き](./systemverilog-reference#celox-execution-model)

## Veryl が出力する SystemVerilog

Veryl の適合性スイートは、Veryl が出力した SystemVerilog も検証します。
出力できないケースや言語間で意味が異なるケースは、[残る対象外のケース](./systemverilog-reference#systemverilog-emitted-by-veryl)を参照してください。
