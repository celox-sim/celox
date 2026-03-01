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

## テストベンチヘルパー

`Simulation` クラスは、よくあるテストベンチパターン向けの便利メソッドを提供します。

### リセットヘルパー

アクティブレベルは Veryl のポート型（`reset`、`reset_async_high`、`reset_async_low` など）から自動的に判定されるため、極性を手動で指定する必要はありません。

```typescript
const sim = Simulation.create(Counter);
sim.addClock("clk", { period: 10 });

// rst を 2 サイクル（デフォルト）アサートしてから解除
sim.reset("rst");

// カスタム: 3 クロックサイクル間リセットを保持
sim.reset("rst_n", { activeCycles: 3 });
```

### 条件待ち

```typescript
// 条件が満たされるまで待つ (step() でポーリング)
const t = sim.waitUntil(() => sim.dut.done === 1);

// 指定クロックサイクル数だけ待つ
const t = sim.waitForCycles("clk", 10);
```

どちらのメソッドもオプションの `{ maxSteps }` パラメータ（デフォルト: 100,000）を受け取ります。ステップ上限を超えると `SimulationTimeoutError` がスローされます：

```typescript
import { SimulationTimeoutError } from "@celox-sim/celox";

try {
  sim.waitUntil(() => sim.dut.done === 1, { maxSteps: 1000 });
} catch (e) {
  if (e instanceof SimulationTimeoutError) {
    console.log(`時刻 ${e.time} で ${e.steps} ステップ後にタイムアウト`);
  }
}
```

### runUntil のタイムアウトガード

`runUntil()` に `{ maxSteps }` を渡すとステップカウントが有効になります。指定しない場合は高速な Rust パスがそのまま使われ、オーバーヘッドはありません：

```typescript
// 高速 Rust パス (オーバーヘッドなし)
sim.runUntil(10000);

// ガード付き: 上限を超えると SimulationTimeoutError をスロー
sim.runUntil(10000, { maxSteps: 500 });
```

## シミュレータオプション

`Simulator` と `Simulation` の両方で以下のオプションが使えます：

```typescript
const sim = Simulator.fromSource(source, "Top", {
  fourState: true,      // 4 値 (X/Z) シミュレーションを有効化
  vcd: "./dump.vcd",    // VCD 波形出力を書き出す
  optimize: true,       // Cranelift 最適化パスを有効化
  clockType: "posedge", // クロック極性 (デフォルト: "posedge")
  resetType: "async_low", // リセットタイプ (デフォルト: "async_low")
});
```

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

- [4 値シミュレーション](./four-state.md) -- テストベンチでの X 値の使い方。
- [アーキテクチャ](/internals/architecture) -- シミュレーションパイプラインの詳細。
- [API リファレンス](/api/) -- TypeScript API の完全なドキュメント。
