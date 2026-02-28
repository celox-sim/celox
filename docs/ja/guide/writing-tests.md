# テストの書き方

Celox は 2 つのシミュレーションモードを提供します: 手動クロック制御の**イベントベース**（`Simulator`）と、自動クロック生成の**タイムベース**（`Simulation`）です。

## イベントベースシミュレーション

`Simulator` はクロックティックを直接制御します。組み合わせ回路や、タイミングを細かく制御したい場合に適しています。

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Adder } from "../src/Adder.veryl";

describe("Adder", () => {
  test("adds two numbers", () => {
    const sim = Simulator.create(Adder);

    sim.dut.a = 100;
    sim.dut.b = 200;
    sim.tick();
    expect(sim.dut.sum).toBe(300);

    sim.dispose();
  });
});
```

- `Simulator.create(Module)` は Veryl モジュール定義からシミュレータインスタンスを作成します。
- シグナル値は `sim.dut.<ポート名>` で読み書きします。
- `sim.tick()` でシミュレーションを 1 クロックサイクル進めます。
- `sim.dispose()` でネイティブリソースを解放します。

## タイムベースシミュレーション

`Simulation` はクロック生成を自動的に管理します。クロック付きフリップフロップを持つ順序回路に適しています。

```typescript
import { describe, test, expect } from "vitest";
import { Simulation } from "@celox-sim/celox";
import { Counter } from "../src/Counter.veryl";

describe("Counter", () => {
  test("counts up when enabled", () => {
    const sim = Simulation.create(Counter);

    sim.addClock("clk", { period: 10 });

    // リセットをアサート
    sim.dut.rst = 1;
    sim.runUntil(20);

    // リセットを解除してカウントを有効化
    sim.dut.rst = 0;
    sim.dut.en = 1;
    sim.runUntil(100);

    expect(sim.dut.count).toBeGreaterThan(0);
    expect(sim.time()).toBe(100);

    sim.dispose();
  });
});
```

- `sim.addClock("clk", { period: 10 })` は周期 10（5 時間単位ごとにトグル）のクロックを追加します。
- `sim.runUntil(t)` はシミュレーション時刻を `t` まで進めます。
- `sim.time()` は現在のシミュレーション時刻を返します。

## 型安全なインポート

Vite プラグインが `.veryl` ファイルの TypeScript 型定義を自動生成します。以下のように書くと:

```typescript
import { Counter } from "../src/Counter.veryl";
```

すべてのポートが完全に型付けされ、ポート名の自動補完やコンパイル時チェックが利用でき、シグナル幅に基づいた適切な数値型（`number` または `bigint`）が使用されます。

## テストの実行

```bash
pnpm test
```

## 関連資料

- [4 値シミュレーション](./four-state.md) -- テストベンチでの X/Z 値の使い方。
- [アーキテクチャ](/internals/architecture) -- シミュレーションパイプラインの詳細。
