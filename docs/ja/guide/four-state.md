# 4 値シミュレーション

Celox は IEEE 1800 準拠の 4 値シミュレーションをサポートし、X（不定）値を扱えます。このページでは TypeScript テストベンチから 4 値機能を使う方法を説明します。

## 4 値モードの有効化

シミュレータ作成時に `{ fourState: true }` を渡します：

```typescript
import { Simulator } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulator.create(MyModule, { fourState: true });
```

`Simulation` でも同様に使えます：

```typescript
import { Simulation } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulation.create(MyModule, { fourState: true });
```

::: warning
`fourState: true` を指定しない場合、すべてのシグナルは 2 値として動作します。X 値の書き込み・読み出しはできません。
:::

## Veryl の型と 4 値

シグナルが 4 値に対応するかは Veryl の型によって決まります：

| 型 | ステート | 備考 |
|------|-------|-------|
| `logic` | 4 値 | 基本的な 4 値型 |
| `clock`, `reset` | 4 値 | 制御信号 |
| `bit` | 2 値 | マスクは常に 0 |

設計内部で 4 値の値が `bit` 型の変数に代入されると、マスク（X ビット）は自動的に 0 にクリアされます。これにより 2 値の境界を越えた意図しない X 伝搬が防止されます。

::: tip
4 値シミュレーションを使うには、ポートを `bit` ではなく `logic` で宣言してください。`bit` で宣言されたポートは X 値を暗黙的に落とします。
:::

## X 値の書き込み

### 全ビットを X にする

`X` センチネルを使ってポートの全ビットを X に設定します：

```typescript
import { Simulator, X } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulator.create(MyModule, { fourState: true });

sim.dut.data_in = X;
sim.tick();
```

### 特定のビットを X にする

`FourState(value, mask)` を使って個別のビットを制御します。マスクのビットが 1 の箇所が X になります：

```typescript
import { Simulator, FourState } from "@celox-sim/celox";
import { MyModule } from "../src/MyModule.veryl";

const sim = Simulator.create(MyModule, { fourState: true });

// ビット [3:0] = 0101、ビット [7:4] = X
sim.dut.data_in = FourState(0b0000_0101, 0b1111_0000);
sim.tick();
```

幅の広いシグナル（53 ビット超）には `bigint` を使います：

```typescript
sim.dut.wide_data = FourState(0x1234n, 0xFF00n);
```

## 4 値の読み出し

### 通常の読み出し

`sim.dut.<ポート名>` で読み出すと **value 部分のみ**が返されます（マスクは含まれません）：

```typescript
const val = sim.dut.result; // number または bigint — X ビットは 0 として読まれる
```

### value/mask ペアの読み出し

`Simulator` または `Simulation` の `fourState()` メソッドで value と X マスクをまとめて読み出せます：

```typescript
const fs = sim.fourState("result");

if (fs.mask !== 0) {
  console.log("結果に X ビットが含まれています:", fs.mask.toString(2));
}

// fs.__fourState === true
// fs.value — value 部分
// fs.mask  — マスク (1 = X)
```

返される `FourStateValue` の各ビットの意味は以下の通りです：

| mask ビット | value ビット | 意味 |
|----------|-----------|---------|
| 0 | 0 | `0` |
| 0 | 1 | `1` |
| 1 | 0 | `X` |

::: tip
低レベルアクセス（カスタムバッファ操作など）が必要な場合は、内部 API の `readFourState(buffer, layout)` 関数も利用できます。
:::

## 例：X 伝搬のテスト

以下のようなプロジェクト構成を想定します：

```
my-project/
├── src/
│   └── ALU.veryl          # logic ポートを持つ設計
└── test/
    └── alu.test.ts        # 4 値テスト
```

`src/ALU.veryl` では `logic` 型のポート（4 値対応）を使います：

```veryl
module ALU (
    clk: input  clock,
    rst: input  reset,
    a:   input  logic<8>,
    b:   input  logic<8>,
    op:  input  logic<2>,
    y:   output logic<8>,
) {
    // ...
}
```

`test/alu.test.ts`:

```typescript
import { describe, test, expect } from "vitest";
import { Simulator, X, FourState } from "@celox-sim/celox";
import { ALU } from "../src/ALU.veryl";

describe("ALU", () => {
  test("X 入力が出力に伝搬する", () => {
    const sim = Simulator.create(ALU, { fourState: true });

    sim.dut.a = X;
    sim.dut.b = 42;
    sim.tick();

    // X を含む算術演算は結果が全て X になる
    // readFourState で X 伝搬を確認
    // ...

    sim.dispose();
  });

  test("既知の 0 との AND は X を打ち消す", () => {
    const sim = Simulator.create(ALU, { fourState: true });

    // a = X だが b = 0 — AND の結果は既知の 0 になる
    sim.dut.a = X;
    sim.dut.b = 0;
    sim.dut.op = 0; // AND
    sim.tick();

    sim.dispose();
  });

  test("FourState で部分的な X を設定", () => {
    const sim = Simulator.create(ALU, { fourState: true });

    // 下位 4 ビットは既知、上位 4 ビットは X
    sim.dut.a = FourState(0x05, 0xF0);
    sim.dut.b = 0xFF;
    sim.tick();

    sim.dispose();
  });
});
```

## X 伝搬ルール

Celox は IEEE 1800 の X 伝搬セマンティクスに従います：

| 演算 | 動作 |
|-----------|----------|
| `a & b` | 既知の `0` が X を打ち消す |
| `a \| b` | 既知の `1` が X を打ち消す |
| `a ^ b` | どちらかのオペランドが X なら結果も X |
| `+`, `-`, `*`, `/`, `%` | オペランドに X が含まれると結果全体が X |
| `==`, `!=`, `<`, `>` | オペランドに X が含まれると結果が X |
| `if (x_cond)` | X セレクタは両ブランチを保守的にマージ |
| X 量のシフト | 結果全体が X |

## 関連資料

- [4 値シミュレーションの内部実装](/internals/four-state) -- 表現モデル、正規化、JIT コンパイルの詳細。
