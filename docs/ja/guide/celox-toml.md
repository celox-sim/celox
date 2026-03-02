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
