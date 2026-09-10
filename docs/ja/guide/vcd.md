# VCD 波形出力

Celox は [GTKWave](https://gtkwave.sourceforge.net/) や [Surfer](https://surfer-project.org/) などのツールで波形を確認するための [VCD（Value Change Dump）](https://en.wikipedia.org/wiki/Value_change_dump) ファイルを出力できます。

## 基本的な使い方

シミュレータ作成時に `vcd` オプションで出力先ファイルパスを指定します：

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  vcd: "./dump.vcd",
});
```

記録したいタイミングで `sim.dump(timestamp)` を呼び出します：

```typescript
sim.dut.a = 10n;
sim.dump(0);      // t=0 の状態を記録

sim.dut.a = 20n;
sim.tick();
sim.dump(10);     // t=10 の状態を記録

sim.dispose();    // ファイルをフラッシュして閉じる
```

::: warning
VCD 出力はバッファリングされます。残りのデータを書き出すため、終了時は必ず `dispose()` を呼ぶか、try/finally ブロックで確実に実行してください。
:::

Rust では `Simulator::flush_vcd()` または `Simulation::flush_vcd()` を呼ぶと、
シミュレータの破棄前に出力を反映し、書き込みエラーを処理できます。
単独の `VcdWriter` では `flush()` と `into_inner()` を使用できます。

## タイムベースシミュレーションでの使い方

`Simulation` では `sim.time()` で現在時刻を取得して `dump()` に渡します：

```typescript
const sim = Simulation.fromSource(SOURCE, "Top", {
  vcd: "./dump.vcd",
});
sim.addClock("clk", { period: 10 });

sim.reset("rst");
sim.dump(sim.time());

sim.dut.en = 1n;
sim.runUntil(100);
sim.dump(sim.time());

sim.dispose();
```

## 関連資料

- [テストの書き方](./writing-tests.md) -- Simulator・Simulation のパターン。
