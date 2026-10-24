# celox.toml

`celox.toml` はプロジェクトルートの `Veryl.toml` と並べて置く、Celox 固有のオプション設定ファイルです。シミュレーションとテストにのみ関係する設定で Veryl プロジェクトを拡張します。

## なぜ必要か

`Veryl.toml` の `[build] sources` リストは、本番ビルドでコンパイルするディレクトリを制御します。テスト専用モジュール（ヘルパーフィクスチャ、モック周辺機器、参照モデルなど）は、そのビルドに含めるべきではありません。

`celox.toml` を使うと、Celox が**シミュレーションと型生成のときだけ**読み込む追加ソースディレクトリを宣言でき、標準の Veryl ビルドから切り離せます。

## ファイル構成

`celox.toml` は `Veryl.toml` の隣に置きます：

```
my-project/
├── Veryl.toml
├── celox.toml          ← Celox 設定
├── src/
│   └── Adder.veryl     # 本番ソース（Veryl.toml に記載）
└── test_veryl/
    └── Reg.veryl        # テスト専用ソース（celox.toml に記載）
```

## 設定リファレンス

```toml
[test]
sources = ["test_veryl"]

[simulation]
max_steps = 100000
```

### `[test]`

| キー | 型 | 説明 |
|---|---|---|
| `test.sources` | `string[]` | シミュレーションと型生成に含める `.veryl` ファイルが存在するディレクトリ（`celox.toml` からの相対パス）。 |

### `[simulation]`

| キー | 型 | デフォルト | 説明 |
|---|---|---|---|
| `simulation.max_steps` | `integer` | 100,000 | `waitUntil` と `waitForCycles` のデフォルトステップ上限。この回数以内に条件が満たされない場合、`SimulationTimeoutError` がスローされます。呼び出し時に `{ maxSteps }` を指定するとこの値を上書きできます。 |

## 例

**`Veryl.toml`** — 本番ビルド、`src/` のみ含む：

```toml
[project]
name    = "my_project"
version = "0.1.0"

[build]
clock_type = "posedge"
reset_type = "async_low"
sources    = ["src"]
```

**`celox.toml`** — シミュレーション時に `test_veryl/` も追加で読み込み、プロジェクト全体のステップ上限を設定する：

```toml
[test]
sources = ["test_veryl"]

[simulation]
max_steps = 50000
```

**`test_veryl/Reg.veryl`** — テスト専用モジュール：

```veryl
module Reg (
    clk: input  clock,
    rst: input  reset,
    d:   input  logic<8>,
    q:   output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
        } else {
            q = d;
        }
    }
}
```

**`test/reg.test.ts`** — テストは `Reg` を通常のモジュールと同様にインポートする：

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Reg } from "../test_veryl/Reg.veryl";

describe("Reg", () => {
  test("立ち上がりエッジで入力を取り込む", () => {
    const sim = Simulator.create(Reg);

    sim.dut.d = 0xABn;
    sim.tick();
    expect(sim.dut.q).toBe(0xABn);

    sim.dispose();
  });
});
```

Vite プラグインは `test_veryl/` を自動的に検出し、そこで宣言されたすべてのモジュールの型定義を生成します。

## 動作

- `celox.toml` が存在しない場合、Celox は `Veryl.toml` に記載されたソースのみを使用し、シミュレーション設定はすべてビルトインのデフォルト値にフォールバックします。
- すべてのテストソースディレクトリはシミュレーション時にプロジェクトソースと統合されます。両方のモジュールが同じ名前空間で利用できます。
- Vite プラグインは本番ソースと同様に、ホットリロード時にテストソースの型も再生成します。
- `[simulation]` の設定は、プロジェクト内の `Simulation.fromProject` / `Simulation.create` 呼び出しすべてに適用されます。呼び出し時に `{ maxSteps }` を指定すると、常に `celox.toml` の値より優先されます。


### CLI の SystemVerilog 時間設定

`celox run` と `celox vpi build` では、ルートまたは `[targets.NAME]` に
`timescale = "1ns/1fs"` を指定できます。優先順位は `--timescale 1ns/1fs`、
選択ターゲット、ルート設定の順です。未指定時は従来どおり `1ns/1ps` です。

時間宣言のないスコープの既定単位・精度を設定し、指定した精度を全体の
スケジューラ精度にも反映します。SV モジュールが `1ns/1ps` を宣言していても、
cocotb から fs 単位のクロックを駆動できます。明示された `timeunit`、
`timeprecision`、`` `timescale `` によるスコープ内の単位と遅延の丸めは維持します。
全体の精度は、設定とソース宣言のうち最も細かい値になります。値は倍率
1・10・100 と単位 `s`・`ms`・`us`・`ns`・`ps`・`fs` を使い、精度を単位より
粗くする指定はエラーです。

### `[diagnostics]`

診断に表示されるコードを使って、コンパイル時の診断レベルを個別に変更できます。

```toml
[diagnostics]
mixed_timescale = "warn"
inferred_latch = "warn"
exit_outside_program = "allow"
memory_file_word_count = "error"
```

`allow` は診断を抑制し、`warn` は警告を残してコンパイルを続行し、`error` はコンパイルを失敗させます。指定しない診断は既定のレベルを維持します。既存の Veryl・SystemVerilog の警告にも適用できます。診断に表示されたコードをそのまま指定してください。そのコンパイルで発生しないコードの設定は影響しません。

エラーの緩和は、コンパイラが継続時の動作を定義している診断に対応します。`mixed_timescale` はその一つです。IEEE 1800-2023 §3.14.2.3 が求める時間単位・精度の明示指定の一貫性を検査し、既定では `error` になります。`warn` または `allow` を選ぶと、未指定部分には Celox の既定値（単位 1ns、精度 1ps）を適用します。明示した値は維持し、全体の精度には既定値を使うスコープも含めます。構文エラー、未対応の構文、不正な精度指定は引き続きエラーになります。

`inferred_latch` は SystemVerilog と Veryl の `always_comb` で値を保持する代入を既定では拒否します。
`warn` ならソース位置付きの警告を出してラッチをシミュレーションし、`allow` なら警告を出さずに許可します。
通常の `always @*` と明示的な `always_latch` にはこの緩和は不要です。
ラッチのノンブロッキング代入と未対応の組み合わせフィードバックは引き続きエラーになります。
Rust の `.allow_inferred_latches(true)` と JavaScript の `allowInferredLatches: true` は、
この診断の明示設定がない場合だけ `allow` を補います。明示した `error`・`warn`・`allow` が優先されます。

`interface_array_query` の既定値は `error` です。`warn` または `allow` で、固定長の interface 配列と添字で選択した部分配列に対する `$size` を互換拡張として許可します。IEEE 1800-2023 §20.7 の対象はデータ配列であり、interface インスタンスはデータオブジェクトではありません。省略可能な次元引数は既知の整数定数に限ります。この拡張によって interface ハンドルに packed 幅が付いたり、他の配列クエリが使えるようになったりすることはありません。

```toml
[diagnostics]
interface_array_query = "warn"
```

CLI でも同じ設定を使います: `celox run top.sv --top Top --diagnostic interface_array_query=warn`。

`multiple_always_ff_drivers` は、`always_ff` と別のエッジプロセスが同じメモリへ書く場合に既定で `error` になります（IEEE 1800-2023 §9.2.2.4）。`warn` または `allow` では、非ブロッキング代入の変更範囲を次状態領域に保持してコミットします。通常の `always` 同士による共有にはこの診断は出ません。異なるプロセスが同時に同じビットへ書く順序は、移植可能な動作として保証されません。

プロジェクトのシミュレータ生成時は `Veryl.toml` と同じ場所の設定を読み込みます。CLI は単独の SystemVerilog プロジェクトにも対応します。

```sh
celox run bus.sv top.sv --top Top --diagnostic mixed_timescale=warn
celox vpi build --project ./celox.toml --target compat --diagnostic exit_outside_program=allow
```

両コマンドは [CLI ガイド](./cocotb.md) のプロジェクト探索・パス解決の規則を使います。ソースを明示した場合は `--project FILE` で設定を読み込みます。入力の指定がない場合、または `--target NAME` を指定した場合は、カレントディレクトリから親へ `celox.toml` を探索します。ターゲットごとに一部の診断だけ上書きすることもできます。

```toml
sources = ["bus.sv", "top.sv"]
top = "Top"

[diagnostics]
mixed_timescale = "error"

[targets.compat.diagnostics]
mixed_timescale = "warn"
```

共通設定→選択したターゲット→CLI の順に優先されます。`--diagnostic CODE=LEVEL` は繰り返し指定でき、同じコードを複数回指定した場合は最後の指定が有効です。`celox run` と `celox vpi build` は抑制していない警告を表示します。診断レベルを変更しても、最適化設定や実行時エラーの扱いは変わりません。

## CLIの最適化と信号アクセス

`celox run` と `celox vpi build` は、共通設定とターゲット設定に次の項目を指定できます。

```toml
sources = ["top.sv"]
top = "Top"
opt_level = "O2"
live_signals = ["Top.uut.debug_count"]
force_signals = ["Top.uut.state"]

[targets.observe]
opt_level = "O1"
force_signals = []
```

最適化レベルの既定値は `celox run` では `O2`、`celox vpi build` では `O1` です。信号リストの既定値は空です。`O2` では出力に影響しない内部の計算を削除することがあるため、外部から観測する信号を `live_signals` で保持します。`force_signals` は保持対象にもなり、シンボリック評価とSIR最適化より前に外部から値を切り替える制御を追加します。これにより、blocking代入と後続の式もforceされた値を参照できます。設計全体を `O0` に変更することはありません。forceの許可対象は生成した実行バイナリにも保存され、宣言外へのforceは失敗します。

パスはtopを含むVPI上の完全名を指定します（例: `Top.uut.s_axis.tdata`）。存在しない名前や曖昧な名前はコンパイルエラーとなり、ワイルドカードは展開しません。明示的なforceは、現在SystemVerilogのpacked変数全体とinterfaceが所有するメンバーに対応しています。interfaceメンバーは子のmodport側の別名ではなく、所有する変数を指定してください。構造化ポートの別名をforce対象にするとエラーになります。unpacked配列、clock/resetのイベント信号、Verylまたは混在設計のforce対象はエラーになります。観測対象の指定は両方の言語に対応しています。

```sh
celox vpi build --project celox.toml -O O2 \
  --live-signal Top.uut.debug_count --force-signal Top.uut.state -o dut-sim
celox run --project celox.toml --target observe --no-force-signals
```

共通設定→選択したターゲット→CLIの順に優先されます。信号リストは全体を置き換えます。ターゲットで省略したリストは共通設定を継承し、`[]` で空にできます。CLIでは `--live-signal` / `--force-signal` を繰り返して列挙し、`--no-live-signals` / `--no-force-signals` で継承したリストを空にできます。リストを空にするオプションと、対応するパス指定は併用できません。これらの設定はCLIの2つのコンパイルコマンドに適用されます。JavaScriptのシミュレータオプションは別に指定します。

## CLIのtopパラメータ指定

`run` と `vpi build` では、`-G NAME=VALUE` または `--parameter NAME=VALUE` を繰り返し指定できます（`--param` も同じ意味です）。選択したtopモジュールの数値パラメータをエラボレーション前に上書きします。SystemVerilog・Verylの両方で使え、ソースの書き換えは不要です。

```sh
celox vpi build --project celox.toml -G ID_EN=1 -G DEST_EN=1 -o dut-sim
```

```toml
sources = ["top.sv"]
top = "Top"

[parameters]
ID_EN = 1
DEST_EN = 1
DATA_W = 8

[targets.wide.parameters]
DATA_W = 64
```

共通設定→選択したターゲット→CLIの順に、パラメータ名ごとに上書きします。同じ名前の最後の指定が有効になり、他の名前の指定は継承します。値は符号なし64bit整数で、10進数・`0x`（16進）・`0o`（8進）・`0b`（2進）を使えます。`0x12_34` のような桁区切りも使えます。TOMLでは整数または文字列（`DATA_W = "0x40"`）で指定し、TOMLの符号付き64bit整数の範囲を超える値は文字列にします。負数、HDLの式やリテラル、HDLの文字列値、型パラメータには対応していません。型変換と切り詰めには、HDL側で宣言したパラメータの型が使われます。不正な指定や存在しないパラメータ名はエラーになります。
