# テストの書き方

Celox は 2 つのシミュレーションモードを提供します: 手動クロック制御の**イベントベース**（`Simulator`）と、自動クロック生成の**タイムベース**（`Simulation`）です。

## イベントベースシミュレーション

`Simulator` はクロックティックを直接制御します。クロックエッジをステップごとに明示的に駆動したい場合に使います。

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@celox-sim/celox";
import { Reg } from "../src/Reg.veryl";

describe("Reg", () => {
  test("クロックエッジで入力をキャプチャする", () => {
    const sim = Simulator.create(Reg);

    // 値をセットしてクロックを入れる
    sim.dut.d = 0xABn;
    sim.tick();
    expect(sim.dut.q).toBe(0xABn);

    // 入力を変えても次の tick までは出力が変わらない
    sim.dut.d = 0xCDn;
    expect(sim.dut.q).toBe(0xABn);
    sim.tick();
    expect(sim.dut.q).toBe(0xCDn);

    sim.dispose();
  });
});
```

`src/Reg.veryl`:

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

- `Simulator.create(Module)` は Veryl モジュール定義からシミュレータインスタンスを作成します。
- シグナル値は `sim.dut.<ポート名>` で読み書きします。
- `sim.tick()` でシミュレーションを 1 クロックサイクル進めます。
- `sim.dispose()` でネイティブリソースを解放します。

::: tip 組み合わせ回路の場合
`always_comb` だけのモジュールでは `tick()` は不要です。出力を読むと現在の入力で組み合わせロジックが自動的に評価されます。
:::

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
    sim.dut.rst = 1n;
    sim.runUntil(20);

    // リセットを解除してカウントを有効化
    sim.dut.rst = 0n;
    sim.dut.en = 1n;
    sim.runUntil(100);

    expect(sim.dut.count).toBeGreaterThan(0n);
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
const t = sim.waitUntil(() => sim.dut.done === 1n);

// 指定クロックサイクル数だけ待つ
const t = sim.waitForCycles("clk", 10);
```

どちらのメソッドもオプションの `{ maxSteps }` パラメータを受け取ります。デフォルトのステップ上限は 100,000 ですが、[`celox.toml` の `[simulation] max_steps`](./celox-toml.md#simulation) でプロジェクト全体に設定できます。ステップ上限を超えると `SimulationTimeoutError` がスローされます：

```typescript
import { SimulationTimeoutError } from "@celox-sim/celox";

try {
  sim.waitUntil(() => sim.dut.done === 1n, { maxSteps: 1000 });
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
  fourState: true,      // 4 値 (X) シミュレーションを有効化
  vcd: "./dump.vcd",    // VCD 波形出力を書き出す
  optimize: true,       // Cranelift 最適化パスを有効化
  clockType: "posedge", // クロック極性 (デフォルト: "posedge")
  resetType: "async_low", // リセットタイプ (デフォルト: "async_low")
  parameters: [         // トップレベルパラメータのオーバーライド
    { name: "WIDTH", value: 16 },
  ],
});
```

## 型安全なインポート

Vite プラグインが `.veryl` ファイルの TypeScript 型定義を自動生成します。以下のように書くと:

```typescript
import { Counter } from "../src/Counter.veryl";
```

すべてのポートが完全に型付けされ、ポート名の自動補完やコンパイル時チェックが利用できます。すべてのシグナルポート値は `bigint` を使用します。

## テストの実行

```bash
pnpm test
```

## ファクトリメソッドの使い分け

3 つのファクトリメソッドはすべて同等のシミュレータを生成します。違いはソースの取得元だけです。

**`Simulator.create(Module)` / `Simulation.create(Module)`** は Vite プラグインを使う場合の標準的な選択肢です。インポートした `Module` にプロジェクトパスが埋め込まれているため、`create` は内部で `fromProject` に委譲し、全ソースファイルと `Veryl.toml` の設定を自動的に読み込みます。ポートの型も生成済みです。

```typescript
import { Adder } from "../src/Adder.veryl"; // Vite プラグインが生成
const sim = Simulator.create(Adder);
```

**`fromProject(path, name)`** は `create` と同じ動作ですが、パスを明示的に指定します。Vite ビルド外の Node.js スクリプトなど、静的インポートなしにプロジェクトディレクトリを指定したい場合に使います。

```typescript
const sim = Simulator.fromProject("./my-project", "Adder");
```

**`fromSource(source, name)`** は `Veryl.toml` なしで Veryl ソース文字列を直接コンパイルします。クロックとリセットの設定はオプションで個別に指定する必要があります。完全に自己完結したテストや、設計をテストファイル内にインラインで書く場合に便利です。

```typescript
const SOURCE = `
module Adder ( ... ) { ... }
`;
const sim = Simulator.fromSource(SOURCE, "Adder", {
  clockType: "posedge",
  resetType: "async_low",
});
```

## 関連資料

- [4 値シミュレーション](./four-state.md) -- テストベンチでの X 値の使い方。
- [パラメータオーバーライド](./parameter-overrides.md) -- シミュレーション時にモジュールパラメータを上書きする方法。
- [アーキテクチャ](/internals/architecture) -- シミュレーションパイプラインの詳細。
- [API リファレンス](/api/) -- TypeScript API の完全なドキュメント。
