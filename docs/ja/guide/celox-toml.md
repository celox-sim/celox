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

### `[diagnostics]`

診断に表示されるコードを使って、コンパイル時の診断レベルを個別に変更できます。

```toml
[diagnostics]
mixed_timescale = "warn"
exit_outside_program = "allow"
memory_file_word_count = "error"
```

`allow` は診断を抑制し、`warn` は警告を残してコンパイルを続行し、`error` はコンパイルを失敗させます。指定しない診断は既定のレベルを維持します。既存の Veryl・SystemVerilog の警告にも適用できます。診断に表示されたコードをそのまま指定してください。そのコンパイルで発生しないコードの設定は影響しません。

エラーの緩和は、コンパイラが継続時の動作を定義している診断に対応します。現在の対象は `mixed_timescale` です。IEEE 1800-2023 §3.14.2.3 が求める時間単位・精度の明示指定の一貫性を検査し、既定では `error` になります。`warn` または `allow` を選ぶと、未指定部分には Celox の既定値（単位 1ns、精度 1ps）を適用します。明示した値は維持し、全体の精度には既定値を使うスコープも含めます。構文エラー、未対応の構文、不正な精度指定は引き続きエラーになります。

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
