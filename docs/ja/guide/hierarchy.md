# 子インスタンスへのアクセス

Veryl モジュールがサブモジュールをインスタンス化している場合、そのポートにはインスタンス名を使って DUT アクセサから参照できます。

## 基本的な使い方

サブモジュールをインスタンス化した設計を例にします：

```veryl
module Sub (
    clk:    input  clock,
    i_data: input  logic<8>,
    o_data: output logic<8>,
) {
    always_comb {
        o_data = i_data;
    }
}

module Top (
    clk:     input  clock,
    rst:     input  reset,
    top_in:  input  logic<8>,
    top_out: output logic<8>,
) {
    inst u_sub: Sub (
        clk,
        i_data: top_in,
        o_data: top_out,
    );
}
```

`u_sub` のポートには `sim.dut.u_sub` でアクセスできます：

```typescript
const sim = Simulator.fromSource(SOURCE, "Top");

sim.dut.top_in = 0xABn;
sim.tick();

// トップレベルの出力
expect(sim.dut.top_out).toBe(0xABn);

// 同じ値を子インスタンスアクセサ経由で確認
expect((sim.dut as any).u_sub.o_data).toBe(0xABn);
expect((sim.dut as any).u_sub.i_data).toBe(0xABn);

sim.dispose();
```

子インスタンスアクセサは、デバッグ時にトップレベルの出力ポートを追加しなくても内部信号を観測するのに役立ちます。

## Vite プラグインを使った型安全なアクセス

Vite プラグインを使う場合、生成される型定義に子インスタンスがネストされたオブジェクト型として含まれます：

```typescript
export interface TopPorts {
  top_in: bigint;
  readonly top_out: bigint;
  readonly u_sub: {
    i_data: bigint;
    readonly o_data: bigint;
  };
}
```

そのままキャストなしで参照できます：

```typescript
import { Top } from "../src/Top.veryl";
const sim = Simulator.create(Top);

expect(sim.dut.u_sub.o_data).toBe(0xABn); // 型付き、キャスト不要
```

## `fromSource` を使った型安全なアクセス

`fromSource` を使う場合は、インターフェースを自分で定義します：

```typescript
interface TopPorts {
  top_in: bigint;
  readonly top_out: bigint;
  readonly u_sub: {
    i_data: bigint;
    readonly o_data: bigint;
  };
}

const sim = Simulator.fromSource<TopPorts>(SOURCE, "Top");
expect(sim.dut.u_sub.o_data).toBe(0xABn);
```

## ネストした階層

子アクセサは再帰的に動作するため、深くネストしたインスタンスも同様にアクセスできます：

```typescript
sim.dut.u_mid.u_inner.some_port
```

## DSE と子インスタンス

[デッドストア除去](./dead-store-elimination.md)が有効な場合、子インスタンスアクセサが利用できないことがあります：

- `deadStorePolicy: "preserveTopPorts"` — 子インスタンスは DUT から除外され、`sim.dut.u_sub` は `undefined` になります。
- `deadStorePolicy: "preserveAllPorts"` — すべてのインスタンスのポートが保持され、子アクセサは通常通り動作します。

DSE を有効にしつつサブインスタンスの信号を観測する場合は `"preserveAllPorts"` を使ってください。

## 関連資料

- [デッドストア除去](./dead-store-elimination.md) -- DSE ポリシーと子アクセスへの影響。
- [テストの書き方](./writing-tests.md) -- Simulator・Simulation のパターン。
- [型変換](./type-conversion.md) -- Veryl の型と TypeScript の型のマッピング。
