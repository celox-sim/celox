# TypeScript テストベンチ — 目指す体験

## 背景と動機

現在の Veryl シミュレータは Rust API を提供しており、`simulator-macros` クレートの proc macro によって Veryl モジュール定義から型安全なアクセサを自動生成している。この体験は強力だが、Rust のコンパイル時間やボイラープレートが検証の反復速度を制約する場面がある。

TypeScript には以下の抗えない強みがある:

1. **型システム + import による自動生成**: `.veryl` ファイルから `.d.ts` 型定義を生成すれば、`import` した瞬間にポート名・ビット幅・型が補完される。ユーザーが意識せずとも型安全が実現する
2. **Python より速い可能性**: V8/Bun の JIT コンパイルにより、テストベンチのオーケストレーション層は Python (cocotb 等) より高速に動作しうる
3. **即座のフィードバック**: コンパイル不要で `bun test` や `vitest` で即座に実行可能。ホットリロードも活用できる
4. **圧倒的な IDE 体験**: VS Code の TypeScript サポートにより、ポート名補完・型エラー・リファクタリングが最初から動く

## 目指す体験

### 1. import するだけで型が付く

```typescript
// Veryl モジュール定義から型が自動生成される
import { Adder } from "./Adder.veryl";

// Adder の型情報:
//   clk: clock
//   rst: reset
//   a: input logic<16>
//   b: input logic<16>
//   sum: output logic<17>
```

ユーザーは `.veryl` ファイルを import するだけで、そのモジュールのポート定義が TypeScript の型として利用可能になる。手動で型定義を書く必要はない。

### 2. 直感的な DUT 操作

```typescript
import { Simulator } from "@veryl-lang/simulator";
import { Adder } from "./Adder.veryl";

const sim = await Simulator.create(Adder);
const dut = sim.dut;

// プロパティ代入でポート値を設定（型チェック付き）
dut.a = 100;
dut.b = 200;

// tick でクロックを進める
await sim.tick();

// プロパティ参照で出力を取得
console.log(dut.sum); // => 300
```

#### 設計原則

- **プロパティアクセス**: `dut.a = 100` / `dut.sum` — `set_a()` / `get_sum()` のような冗長な API は避ける
- **型によるビット幅保証**: `logic<8>` のポートに `256` を代入すると型エラーまたはランタイムエラー
- **await で時間の進行を明示**: `await sim.tick()` は非同期で、将来的な波形ストリーミングやデバッガ連携に対応しやすい

### 3. テストフレームワーク統合

```typescript
import { describe, test, expect } from "vitest";
import { Simulator } from "@veryl-lang/simulator";
import { Counter } from "./Counter.veryl";

describe("Counter", () => {
  test("counts up on each clock edge", async () => {
    const sim = await Simulator.create(Counter);
    const dut = sim.dut;

    dut.rst = 1;
    await sim.tick();
    expect(dut.count).toBe(0);

    dut.rst = 0;
    await sim.tick();
    expect(dut.count).toBe(1);

    await sim.tick();
    expect(dut.count).toBe(2);
  });

  test("resets to zero", async () => {
    const sim = await Simulator.create(Counter);
    const dut = sim.dut;

    dut.rst = 0;
    await sim.tick(5); // 5 サイクル進める
    expect(dut.count).toBe(5);

    dut.rst = 1;
    await sim.tick();
    expect(dut.count).toBe(0);
  });
});
```

- vitest / Jest / Bun test など、既存の JS テストランナーがそのまま使える
- 新しいテストフレームワークを学ぶ必要がない
- CI/CD パイプラインに自然に統合できる

### 4. マルチクロック・高度な制御

```typescript
const sim = await Simulator.create(DualClockFIFO);
const dut = sim.dut;

// 名前付きクロックの明示的操作
const wrClk = sim.event("wr_clk");
const rdClk = sim.event("rd_clk");

dut.wr_data = 0xAB;
dut.wr_en = 1;
await sim.tick(wrClk);

dut.rd_en = 1;
await sim.tick(rdClk);
expect(dut.rd_data).toBe(0xAB);
```

```typescript
// 時間ベースシミュレーション
const sim = await Simulation.create(Top);
sim.addClock("clk", { period: 10 });
sim.schedule("rst", { time: 5, value: 1 });

await sim.runUntil(100);
expect(sim.dut.q).toBe(expectedValue);
```

### 5. 4-state (X/Z) サポート

```typescript
import { X, Z, FourState } from "@veryl-lang/simulator";

const sim = await Simulator.create(Top, { fourState: true });
const dut = sim.dut;

// X/Z を明示的に設定
dut.a = X;                    // 全ビット X
dut.b = FourState(0b1010, 0b0100);  // ビット 2 が X

// X/Z の検査
expect(dut.y).toBeX();       // カスタムマッチャー
expect(dut.y).not.toBeZ();

// ビットレベルの検査
const [val, mask] = sim.getFourState(dut.ref.y);
```

### 6. 波形ダンプ

```typescript
const sim = await Simulator.create(Top, {
  vcd: "./output.vcd",   // VCD 自動出力
});

// テスト実行中の全信号変化が記録される
dut.a = 1;
await sim.tick();
dut.a = 2;
await sim.tick();

sim.dispose(); // ファイルをフラッシュして閉じる
```

### 7. 階層アクセスとインターフェース

```typescript
import { TopModule } from "./Top.veryl";

const sim = await Simulator.create(TopModule);
const dut = sim.dut;

// フラットなトップレベルポート
dut.globalReset = 1;

// インターフェースのメンバーアクセス
dut.bus.addr = 0x1000;
dut.bus.data = 0xFF;
dut.bus.valid = 1;
await sim.tick();
expect(dut.bus.ready).toBe(1);
```

### 8. 配列ポート

```typescript
import { MemoryBank } from "./MemoryBank.veryl";

const sim = await Simulator.create(MemoryBank);
const dut = sim.dut;

// 配列はインデックスアクセス
dut.data[0] = 0xAA;
dut.data[1] = 0xBB;
await sim.tick();
expect(dut.out[0]).toBe(0xAA);
```

### 9. Wide (>64-bit) 値

```typescript
// BigInt でシームレスに扱う
dut.widePort = 0x1234_5678_9ABC_DEF0_1234_5678n;
await sim.tick();
expect(dut.wideOutput).toBe(expectedBigInt);
```

TypeScript は `BigInt` をネイティブサポートしているため、Rust の `BigUint` と同様に任意幅の値を自然に扱える。

## 型生成の仕組み（構想）

```
Veryl ソース (.veryl)
  → veryl analyze (既存の Analyzer + IR)
  → TypeScript 型定義生成器
  → .d.ts ファイル + .js シム (NAPI/WASM 経由で Cranelift バックエンドを呼ぶ)
```

### 生成される型の例

```veryl
module Adder (
    clk: input  clock,
    rst: input  reset,
    a:   input  logic<16>,
    b:   input  logic<16>,
    sum: output logic<17>,
) {
    always_ff (clk, rst) {
        if_reset { sum = 0; }
        else     { sum = a + b; }
    }
}
```

↓ 生成

```typescript
// Adder.d.ts (自動生成)
export interface AdderPorts {
  /** input clock */
  readonly clk: Clock;
  /** input reset */
  rst: number;
  /** input logic<16> — range: 0..65535 */
  a: number;
  /** input logic<16> — range: 0..65535 */
  b: number;
  /** output logic<17> — readonly */
  readonly sum: number;
}

export declare const Adder: ModuleDefinition<AdderPorts>;
```

### import 解決の選択肢

| 方式 | 仕組み | メリット | デメリット |
|------|--------|----------|------------|
| **ビルドステップ生成** | `veryl gen-ts` で `.d.ts` + `.js` を生成 | シンプル、確実 | 手動実行が必要 |
| **TypeScript plugin** | TS Language Service Plugin で `.veryl` を仮想解決 | エディタ上で即時反映 | 実装が複雑 |
| **Bun plugin** | Bun のローダーで `.veryl` を直接 import | ゼロ設定 | Bun 依存 |
| **watch モード** | ファイル変更を監視して自動再生成 | IDE 補完とリアルタイム同期 | プロセス常駐が必要 |

初期実装としては **ビルドステップ生成** が最も現実的。将来的に watch モードや Bun plugin を追加する。

## シミュレータバックエンドの接続方式

TypeScript のテストベンチが Rust/Cranelift ベースのシミュレータエンジンを呼び出す方式:

| 方式 | 概要 | 性能 | 移植性 |
|------|------|------|--------|
| **NAPI (napi-rs)** | Node.js / Bun のネイティブアドオン | 最高速（直接関数呼び出し） | Node.js + Bun |
| **WASM** | Cranelift バックエンドを WASM にコンパイル | 良好（WASM オーバーヘッドあり） | どこでも動く |
| **FFI (Bun)** | Bun の `bun:ffi` で `.so` を直接ロード | NAPI に近い | Bun のみ |

推奨: **NAPI** を最初のターゲットとし、WASM を将来的なポータビリティオプションとして検討。

## ワークフロー全体像

```
1. ユーザーが .veryl ファイルを書く
2. veryl gen-ts を実行（または watch モードで自動実行）
3. .d.ts 型定義が生成される
4. テストベンチを .ts で記述（VSCode が型補完を提供）
5. bun test / vitest で実行
   → NAPI 経由でシミュレータエンジンが起動
   → JIT コンパイルされたネイティブコードで高速シミュレーション
6. VCD 波形を GTKWave 等で確認
```

```
┌──────────────┐    gen-ts     ┌──────────────┐
│  .veryl      │──────────────→│  .d.ts       │
│  (HDL src)   │               │  (型定義)     │
└──────────────┘               └──────┬───────┘
                                      │ import
                                      ▼
                               ┌──────────────┐
                               │  test.ts     │
                               │  (テストベンチ)│
                               └──────┬───────┘
                                      │ bun test / vitest
                                      ▼
                               ┌──────────────┐     NAPI      ┌──────────────┐
                               │  JS Runtime  │──────────────→│  Cranelift   │
                               │  (V8/Bun)    │               │  JIT Engine  │
                               └──────────────┘               └──────────────┘
```

## cocotb (Python) との比較

| | cocotb (Python) | Veryl TS Testbench |
|---|---|---|
| **型安全性** | なし（実行時エラー） | 完全（コンパイル時にポート名・幅を検証） |
| **IDE 補完** | 限定的 | フル補完（TS Language Server） |
| **実行速度** | CPython の制約 | V8/Bun JIT + Cranelift JIT |
| **セットアップ** | VPI/VHPI + 外部シミュレータ必須 | `bun add @veryl-lang/simulator` のみ |
| **シミュレータ** | 外部依存 (Verilator, VCS, etc.) | 内蔵 (Cranelift JIT) |
| **デバッグ** | print + 波形 | VS Code デバッガ + 波形 |
| **テストランナー** | pytest | vitest / Jest / Bun test |
| **ホットリロード** | 不可 | 可能 (vitest watch) |

## 段階的な実装ロードマップ（概要）

### Phase 1: 基盤
- `veryl gen-ts` コマンドで `.d.ts` 型定義を生成
- NAPI バインディングで `Simulator` / `Simulation` クラスを公開
- 基本的なプロパティアクセス（get/set）と `tick()`

### Phase 2: 開発体験
- vitest / Bun test との統合ガイド
- VCD 出力サポート
- 4-state / wide value サポート
- カスタム vitest マッチャー (`toBeX()` 等)

### Phase 3: 高度な機能
- watch モードによる型定義の自動再生成
- Bun plugin による `.veryl` 直接 import
- マルチクロック・時間ベースシミュレーションの TS API
- パフォーマンスベンチマーク vs cocotb

### Phase 4: エコシステム
- npm パッケージとして公開 (`@veryl-lang/simulator`)
- テンプレートプロジェクト (`create-veryl-test`)
- ドキュメント・チュートリアル
