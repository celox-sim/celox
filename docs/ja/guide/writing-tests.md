# テストの書き方

Celox は TypeScript でのテストベンチ記述をサポートしており、モダンな開発ツールを使用して Veryl 設計のシグナルに型安全にアクセスできます。

## 概要

TypeScript テストベンチのワークフローは 3 つの部分で構成されます：

1. **型生成** -- `celox-ts-gen` が Veryl 設計から TypeScript の型定義を生成し、型安全なシグナルアクセサを提供します。
2. **NAPI バインディング** -- `celox-napi` がゼロコピーメモリ共有を使用して、JIT シミュレータランタイムを N-API 経由で Node.js に公開します。
3. **TypeScript ランタイム** -- `@celox-sim/celox` パッケージがシミュレーション駆動のための高レベル API を提供します。

## プロジェクトのセットアップ

NAPI バインディングと TypeScript パッケージがビルドされていることを確認してください：

```bash
pnpm build:napi
pnpm build
```

## テストベンチの記述

典型的なテストベンチでは、シミュレータランタイムと設計用に生成された型をインポートします：

```typescript
import { Simulator } from "@celox-sim/celox";

// Create a simulator instance for your design
const sim = await Simulator.create("path/to/your/design.veryl");

// Access signals with type-safe accessors
sim.dut.clk.set(0n);
sim.dut.reset.set(1n);

// Advance simulation
await sim.step();

// Read signal values
const output = sim.dut.result.get();
```

## クロックとリセット

設計を初期化するためにクロックとリセットシグナルを駆動します：

```typescript
// Assert reset
sim.dut.reset.set(1n);
await sim.tick(5); // Hold reset for 5 clock cycles

// Release reset
sim.dut.reset.set(0n);
await sim.tick(1);
```

## テストの実行

テストは任意の標準的な Node.js テストランナーで実行できます。本プロジェクトでは Vitest を使用しています：

```bash
pnpm test:js
```

## ベンチマーク

リリースビルドでベンチマークを実行するには：

```bash
pnpm bench
```

## 関連資料

- [4 値シミュレーション](/internals/four-state) -- X 値と Z 値の処理方法。
- [アーキテクチャ](/internals/architecture) -- シミュレーションパイプラインの詳細。
