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
- チェックポイントはメモリ上にだけ存在します。プロセスをまたいで状態を残すには、状態ファイルを使います。

## Rust API

Rust の `Simulator` と `Simulation` にも同じ操作があります。

```rust
let checkpoint = sim.checkpoint()?;
sim.restore(&checkpoint)?;
```

上記の制限に当たる場合は、どちらも `CheckpointError` を返します。

## 状態ファイル

状態ファイルは、すべての値を信号のパスで記録するバイナリファイルです。
メモリのレイアウトに依存しないので、あるシミュレータで保存したファイルを、バックエンドや最適化レベルが異なる別のシミュレータに読み込めます。
たとえば、最適化した native ビルドで失敗の直前の状態を保存し、`O0` のインタープリタで再現できます。

`saveState()` はファイルの中身をバイト列で返し、`loadState()` はそれを受け取ります。

```typescript
import { readFileSync, writeFileSync } from "node:fs";

writeFileSync("before_failure.state", sim.saveState());

const other = Simulator.fromSource<CounterPorts>(SOURCE, "Counter", {
  optLevel: "O0",
});
other.loadState(readFileSync("before_failure.state"));
```

状態ファイルは native addon で使えます。WASM ビルドはまだ対応していません。

Rust API では `StateFile` を介して保存・読み込みします。

```rust
let file = sim.save_state()?;
file.write_to(std::fs::File::create("before_failure.state")?)?;

let file = StateFile::read_from(std::fs::File::open("before_failure.state")?)?;
other.load_state(&file)?;
```

読み込むには、設計のすべてのレジスタ・メモリ・入力が同じ幅でファイルに含まれている必要があります。
足りないものがあると何も変更せず、エラーに差分の一覧が入ります。
組み合わせ回路の信号は読み込み後に計算し直すので、一致している必要はありません。
`Simulation` の状態ファイルには、時刻、クロック、予約済みのイベントも名前で記録されます。

状態ファイルは `celox` コマンドで確認できます。

```bash
celox state dump before_failure.state
celox state diff native.state interpreter.state
```

`diff` は、ファイルに差があると終了コード 1 で終わります。
どちらのコマンドも、`--comb` を付けない限り組み合わせ回路の信号を表示しません。
`O2` の dead store elimination は、どこからも読まれない組み合わせ回路の信号を書かないので、その信号の保存値は古いままになっているためです。

## 関連資料

- [テストの書き方](./writing-tests.md) -- Simulator・Simulation のパターン。
- [VCD 波形出力](./vcd.md) -- 波形の記録。
