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

## SystemVerilog からの制御 {#systemverilog-source-controls}

時刻付きシミュレーションでは HDL から波形を指定できます。

```systemverilog
initial begin
    $dumpfile("waves.vcd");
    $dumpvars(0, Top);
end
```

`$dumpvars` は現在時刻の処理が収束してから記録を開始します。すべての呼び出しを
同じシミュレーション時刻に実行してください。深さ `0` は子階層をすべて含み、
`1` は指定モジュール自身の変数だけを含みます。個別変数や複数の選択も指定できます。

`$dumpoff` は不定値を記録して停止し、`$dumpon` は現在値を記録して再開します。
`$dumpall` は記録の停止状態を変えずに選択変数の現在値をすべて記録します。
`$dumpflush` はバッファを出力します。`$dumplimit(bytes)` はサイズ上限に達すると
コメントを出力して記録を停止します。完全なレコードを出力するため上限を多少超える
場合があります。上限 `0` はサイズ制限を解除します。

`$dumpfile` は文字列リテラルまたはファイル名を格納した整数式を受け取り、
`$dumpvars` より前に実行する必要があります。既定のファイル名は `dump.vcd` です。
必要なトレース用ストレージは自動で有効になるため、ホスト側での VCD パス指定は
省略できます。既存のホスト側の波形 API も引き続き使用できます。
