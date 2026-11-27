# SystemVerilog サポート

Celox は **Rust API** から SystemVerilog をシミュレートできます。
ソースファイルとトップモジュールを指定して使います。
Vite プラグインは、現時点では Veryl のみを読み込みます。TypeScript パッケージは、
モジュール定義のソースのうちパスが `.sv` または `.svh` で終わるものを SystemVerilog として
コンパイルします。

Celox は IEEE Std 1800™-2023 に従います。
標準をなぞる代わりに、[対応範囲と逸脱事項のページ](./systemverilog-reference)では
**未対応の機能**、**部分対応の機能**（対応・未対応の対照表）、**個別制約**、
**意図的な逸脱**を分けて整理しています。
記載のない未対応・制限・逸脱に該当しない有効な IEEE 1800-2023 コードは標準どおりに動作します
（見つからない差異は不具合として扱ってください）。既定モードでは既定警告付きの互換緩和をいくつか受理しますが、
strict モード（`strict = true` / `--strict`）で記載のない IEEE 1800-2023 外のコードを受け入れてしまう場合も不具合です。

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

## 対応概要と標準からの逸脱

未対応の構文は、構文名と追跡 issue を含む `Unsupported` エラーで拒否します。
黙って無視することはありません。

| 区分 | 概要 | 詳細 |
| --- | --- | --- |
| **未対応** | checker、`specify` ブロック、UDP、`disable fork` 以外の `disable`、`wait_order`、unpacked / tagged union | [未対応の機能](./systemverilog-reference#what-is-not-supported) |
| **部分対応** | ポート/`ref` ポート/ネット/`defparam`/ゲート、階層参照、interface、virtual interface、clocking ブロック、unpacked 構造体、手続き的 `force`/`assign`、遅延、イベント制御、クラス、ランダム化、動的コンテナ、並行アサーション、covergroup（カバーポイントは自動ビンと値のビン（配列、`default`、`ignore_bins`、`illegal_bins`、`wildcard`、ガード、`with` フィルタ）に対応。クロスは `binsof` と `intersect` の選択に対応。オプションと、covergroup・カバーポイント・クロスの組み込みメソッドに対応。クロッキングイベント（埋め込み covergroup のものを含む）に対応。遷移ビン、クロスの `with` 選択、interface の covergroup のクロッキングイベントは未対応）、I/O の対応・未対応対照表 | [部分対応の機能](./systemverilog-reference#partial-support) |
| **個別制約** | non-ANSI ポート式、`always_comb` / `always_latch` 内のノンブロッキング代入、ループ展開上限などの個別の制限 | [個別制約](./systemverilog-reference#narrow-limitations) |
| **標準準拠コードへの逸脱** | program 継続代入の実行領域、モジュール間 interface の static 記憶域、手続き的 `assign` の上書きタイミング、実行時 `%p`、`$exit` 警告など | [標準準拠コードに影響する逸脱](./systemverilog-reference#deviations-standard-code) |
| **標準外コードの許容** | オプションや既定警告の緩和（`strict = true` / `--strict` でエラー化）および変数選択への `force` や `$psprintf` などの非標準拡張 | [標準外コードの許容](./systemverilog-reference#deviations-nonstandard-code) |

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

- [未対応の機能](./systemverilog-reference#what-is-not-supported)
- [部分対応の機能](./systemverilog-reference#partial-support)
- [個別制約](./systemverilog-reference#narrow-limitations)
- [標準準拠コードに影響する逸脱](./systemverilog-reference#deviations-standard-code)
- [標準外コードの許容](./systemverilog-reference#deviations-nonstandard-code)
- [イベント実行順序の選択](./systemverilog-reference#timing-semantics)
- [時間単位と精度の既定値](./systemverilog-reference#time-and-precision)
- [乱数のシードと再現性](./systemverilog-reference#random-numbers)
- [終了処理と `final` 手続き](./systemverilog-reference#celox-execution-model)
- [メモリファイル（`$readmemh` / `$readmemb`）](./systemverilog-reference#memory-files)

## Veryl が出力する SystemVerilog

Veryl の適合性スイートは、Veryl が出力した SystemVerilog も検証します。
出力できないケースや言語間で意味が異なるケースは、[残る対象外のケース](./systemverilog-reference#systemverilog-emitted-by-veryl)を参照してください。
