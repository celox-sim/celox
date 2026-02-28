# はじめる

## 前提条件

- [Node.js](https://nodejs.org/)（v18 以上）
- [pnpm](https://pnpm.io/)

## プロジェクトのセットアップ

新しいプロジェクトディレクトリを作成して初期化します：

```bash
mkdir my-celox-project && cd my-celox-project
pnpm init
```

Celox と Vitest をインストールします：

```bash
pnpm add -D @celox-sim/celox @celox-sim/vite-plugin vitest
```

### Veryl.toml

プロジェクトルートに `Veryl.toml` を作成します：

```toml
[project]
name    = "my_project"
version = "0.1.0"

[build]
clock_type = "posedge"
reset_type = "async_low"
sources    = ["src"]
```

### vitest.config.ts

```typescript
import { defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
  plugins: [celox()],
});
```

### tsconfig.json

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "bundler",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "allowArbitraryExtensions": true,
    "rootDirs": ["src", ".celox/src"]
  },
  "include": ["test", "src", ".celox/src"]
}
```

## Veryl モジュールを書く

`src/Adder.veryl` を作成します：

```veryl
module Adder (
    clk: input clock,
    rst: input reset,
    a: input logic<16>,
    b: input logic<16>,
    sum: output logic<17>,
) {
    always_comb {
        sum = a + b;
    }
}
```

## テストを書く

`test/adder.test.ts` を作成します：

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

Vite プラグインが `.veryl` ファイルを自動的に解析して TypeScript の型定義を生成するため、`import { Adder } from "../src/Adder.veryl"` のインポートは完全に型付けされます。

## テストを実行する

`package.json` にテストスクリプトを追加します：

```json
{
  "scripts": {
    "test": "vitest run"
  }
}
```

実行します：

```bash
pnpm test
```

## 次のステップ

- [テストの書き方](./writing-tests.md) -- イベントベースとタイムベースのシミュレーションパターン。
- [はじめに](./introduction.md) -- アーキテクチャの概要。
