# チェックポイント

チェックポイントは、実行中のシミュレーションの状態を保存し、あとでその時点に戻れるようにする機能です。
同じシミュレータにも、同じ設計から作った別のシミュレータにも、何度でも復元できます。
リセット直後の状態から多数のシナリオを流したり、長いテストの終盤だけをやり直したりするときに使います。

## イベントベースのシミュレーション

```typescript
const sim = Simulator.fromSource<CounterPorts>(SOURCE, "Counter");
sim.dut.rst = 0n;
sim.tick();
sim.dut.rst = 1n;

const afterReset = sim.checkpoint();

for (const scenario of scenarios) {
  sim.restore(afterReset);
  scenario(sim);
}
```

復元すると、すべてのレジスタ・メモリ・入力が戻り、組み合わせ回路の出力も確定した状態になります。
そのため、復元直後から `sim.dut` で復元後の値を読めます。

シナリオを並行して走らせたいときは、別のシミュレータに復元します。

```typescript
const fork = Simulator.fromSource<CounterPorts>(SOURCE, "Counter");
fork.restore(sim.checkpoint());
```

## 時間ベースのシミュレーション

`Simulation` のチェックポイントには、シミュレーション時刻、登録済みのクロック、予約済みのイベントも含まれます。

```typescript
const sim = Simulation.fromSource<CounterPorts>(SOURCE, "Counter");
sim.addClock("clk", { period: 10 });
sim.runUntil(100);

const checkpoint = sim.checkpoint();
sim.runUntil(500);

sim.restore(checkpoint);
sim.time(); // 100
```

## 制限

- 別の設計のチェックポイントを渡すと、`restore()` は例外を投げます。
- VCD 出力を有効にしていると、波形の時刻を巻き戻せないため、`restore()` は例外を投げます。
- チェックポイント以降に出力された `$display` やアサーションのメッセージは取り消されません。同じサイクルを再実行すると、もう一度出力されます。
- チェックポイントはメモリ上にだけ存在します。ファイルへの保存にはまだ対応していません。

## Rust API

Rust の `Simulator` と `Simulation` にも同じ操作があります。

```rust
let checkpoint = sim.checkpoint()?;
sim.restore(&checkpoint)?;
```

上記の制限に当たる場合は、どちらも `CheckpointError` を返します。

## 関連資料

- [テストの書き方](./writing-tests.md) -- Simulator・Simulation のパターン。
- [VCD 波形出力](./vcd.md) -- 波形の記録。
