# 波形出力（VCD と FST）

Celox は [GTKWave](https://gtkwave.sourceforge.net/) や [Surfer](https://surfer-project.org/) などのツールで波形を確認するための波形ファイルを 2 つの形式で出力できます。

- [VCD（Value Change Dump）](https://en.wikipedia.org/wiki/Value_change_dump)：
  汎用的なテキスト形式。人間が読めますが、長い実行では大きくなります。
- FST（Fast Signal Trace）：GTKWave の圧縮バイナリ形式。同じ信号と時刻を
  はるかに小さいサイズで記録し、ビューアでのランダムアクセスも高速です。
  長いシミュレーションではこちらを推奨します。

どちらの形式も同じ信号を同じタイミングで記録し、このページのオプション・
メソッド・HDL 制御はどちらにも適用されます。`vcd` と `fst` は同時に指定できません。

## CLI

`celox run` に `--vcd <FILE>` または `--fst <FILE>` を指定すると、HDL に
ダンプタスクを追加せずに SystemVerilog・Veryl テストベンチの波形を記録できます。

```sh
celox run top.sv --top Top --backend native --vcd waves.vcd
celox run top.veryl --top Top --fst waves.fst
```

CLI はシミュレーションの各時刻を記録し、テスト失敗やステップ上限による終了時も
ファイルをフラッシュします。波形有効時も適用可能な compiled scheduler を使用し、
波形を記録する境界でホストに戻ります。`--compile-only` との併用はできません。

## 基本的な使い方

シミュレータ作成時に `vcd`（または `fst`）オプションで出力先ファイルパスを指定します：

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  vcd: "./dump.vcd",
  // fst: "./dump.fst",
});
```

Rust では `SimulatorBuilder::vcd` または `SimulatorBuilder::fst` を使います。

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
波形出力はバッファリングされます。残りのデータを書き出すため、終了時は必ず `dispose()` を呼ぶか、try/finally ブロックで確実に実行してください。破棄時に FST ファイルのヘッダも確定します。
:::

Rust では `Simulator::flush_vcd()` または `Simulation::flush_vcd()` を呼ぶと、
シミュレータの破棄前に出力を反映し、書き込みエラーを処理できます。
（名前は VCD のままですが FST もフラッシュし、実行途中でも読めるように
ヘッダを更新します。）単独の `VcdWriter`・`FstWriter` では `flush()` と
`into_inner()` を使用できます。

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
Celox の拡張として（Icarus Verilog の慣習に従い）、`.fst` で終わるファイル名は
標準の VCD 形式ではなく FST で記録します。上の制御は FST でも同様に働き、
`$dumpoff`・`$dumpon` は FST の blackout レコードとしても書かれます。
`$dumplimit` は FST では近似的に適用され、キュー済みの未圧縮変化の分だけ
上限を超えてから記録が止まる場合があります。必要なトレース用ストレージは
自動で有効になるため、ホスト側での波形パス指定は省略できます。
既存のホスト側の波形 API も引き続き使用できます。
