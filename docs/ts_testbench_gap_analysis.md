# TypeScript テストベンチ — 機能ギャップ分析

> 前提: [ts_testbench_vision.md](./ts_testbench_vision.md) のビジョンと [ts_testbench_implementation_plan.md](./ts_testbench_implementation_plan.md) の実装計画に基づく、現状の達成度と不足機能の分析。

## 現在の達成状況

### 実装済み (Phase 1–2 完了)

| 機能 | 状態 | 実装箇所 |
|---|---|---|
| **型生成 (`celox-gen-ts`)** | 完了 | `crates/celox-ts-gen/` |
| **NAPI バインディング** | 完了 | `crates/celox-napi/` |
| **SharedArrayBuffer zero-copy I/O** | 完了 | `crates/celox-napi/src/lib.rs` — `shared_memory()` |
| **DUT Proxy (DataView + dirty tracking)** | 完了 | `packages/celox/src/dut.ts` |
| **Simulator (イベントベース)** | 完了 | `packages/celox/src/simulator.ts` |
| **Simulation (時間ベース)** | 完了 | `packages/celox/src/simulation.ts` |
| **マルチクロック** | 完了 | `sim.event()` + `sim.tick(event)` |
| **addClock / schedule / runUntil / step** | 完了 | `packages/celox/src/simulation.ts` |
| **4-state (X) サポート** | 完了 | `X` sentinel, `FourState()`, DUT 書き込み |
| **vitest カスタムマッチャー** | 完了 | `toBeX()`, `toBeAllX()`, `toBeNotX()` |
| **配列ポート** | 完了 | `.at(i)` / `.set(i, value)` |
| **インターフェース階層アクセス** | 完了 | `dut.bus.addr` 形式 |
| **Wide 値 (>53bit → BigInt)** | 完了 | 自動切り替え |
| **fromSource / fromProject** | 完了 | 両クラスに factory メソッド |
| **Vite プラグイン** | 完了 | `packages/vite-plugin/` |

---

## 機能ギャップ一覧

### 1. VCD 波形出力が TS から設定不可 [Bug]

**深刻度**: 高 — ビジョンで明示された機能が動作しない

`SimulatorOptions.vcd` は TypeScript の型定義に存在するが、NAPI 層に渡されていない。Rust の `SimulatorBuilder.vcd(path)` は正しく動作するが、接続が欠落している。

```
packages/celox/src/types.ts:123   — vcd?: string  (定義あり)
crates/celox-napi/src/lib.rs:15   — NapiOptions { four_state }  (vcd フィールドなし)
```

**現状**: `dump(timestamp)` メソッドは NAPI に公開されているが、VCD Writer の初期化 (`builder.vcd(path)`) が呼ばれないため、`dump()` を呼んでも何も書き出されない。

**修正方針**: `NapiOptions` に `vcd: Option<String>` を追加し、`apply_options` で `builder.vcd()` を呼ぶ。

---

### 2. Rust に実装済みだが TS に未公開の機能

| 機能 | Rust API | NAPI | TS | 備考 |
|---|---|---|---|---|
| **`optimize` フラグ** | `builder.optimize(bool)` | 未公開 | 未公開 | 常に `true` (デフォルト) |
| **`TraceOptions`** | `builder.trace(opts)` + 11 個の個別メソッド | 未公開 | 未公開 | デバッグ/プロファイリング用。CLIF IR、ネイティブ ASM 等 |
| **`build_with_trace()`** | `builder.build_with_trace()` | 未公開 | 未公開 | コンパイルトレース結果を取得 |
| **`false_loop(from, to)`** | `builder.false_loop(...)` | 未公開 | 未公開 | 偽の組み合わせループを break |
| **`true_loop(from, to, max_iter)`** | `builder.true_loop(...)` | 未公開 | 未公開 | 真のループに収束上限を設定 |
| **`next_event_time()`** | `Simulation::next_event_time()` | 未公開 | 未公開 | 次イベントの時刻を先読み |
| **`get_four_state(signal)`** | `Simulator::get_four_state()` | 未公開 | 未公開 | DUT getter は値のみ返す。マスク読み取りは低レベル buffer 操作が必要 |

#### 優先度判定

- **高**: `next_event_time()` — テストベンチでの時間制御に有用
- **中**: `optimize`, `false_loop`, `true_loop` — パワーユーザー向け
- **低**: `TraceOptions` / `build_with_trace` — 開発者デバッグ用

---

### 3. テストベンチとして不足している主要機能

#### 3.1 イベント待ち / 非同期テストベンチフロー [未実装]

**影響**: 大 — 複雑なテストベンチの記述性を著しく制限

現状は全てのシミュレーション制御が同期的。以下の機構が存在しない:

- `@(posedge clk)` 相当の待ち受け
- `waitUntil(condition)` / `waitForSignal(name, value)`
- コールバックベースのイベント登録
- `fork` / `join` による並行刺激
- async generator ベースのテストベンチフロー

**現状の回避策**: `step()` のポーリングループで条件を手動チェック

```typescript
// 理想的な API (未実装)
await sim.waitUntil(() => dut.done === 1);

// 現状の回避策
while (dut.done !== 1) {
  const t = sim.step();
  if (t === null) throw new Error("No more events");
}
```

#### 3.2 信号監視 / 値変化コールバック [未実装]

**影響**: 中

- 信号値の変化をトリガーとするコールバック
- モニタプロセス (`$monitor` 相当)
- 信号プローブ (ポート値以外の内部状態)

#### 3.3 Force / Release [未実装]

**影響**: 中

- 内部信号の強制上書き
- トップレベルポートのみ書き込み可能で、サブモジュール内部には介入不可

#### 3.4 内部信号アクセス [未実装]

**影響**: 中

- DUT はトップレベルポートのみ公開
- サブモジュールの内部変数、レジスタ、配線を参照できない
- デバッグ・ホワイトボックステストに制約

#### 3.5 パラメータオーバーライド [未実装]

**影響**: 中

- Veryl モジュールのパラメータ (例: `param N: u32 = 1000`) を TS 側から変更不可
- 同一モジュールの異なるパラメータ構成をテストするには Veryl ソースを書き換える必要あり

```typescript
// 理想的な API (未実装)
const sim = await Simulator.create(FIFOModule, {
  parameters: { DEPTH: 16, WIDTH: 32 },
});
```

#### 3.6 リセットヘルパー [未実装]

**影響**: 低 — 回避策が容易

自動リセットシーケンスやヘルパーが存在しない。手動で制御する必要がある:

```typescript
// 現状 (冗長だが動作する)
dut.rst = 1;
sim.tick();
dut.rst = 0;
```

#### 3.7 タイムアウト / シミュレーション安全ガード [未実装]

**影響**: 低

- `runUntil()` や `step()` ループの最大時間制限
- 無限ループ防止のガード
- テストタイムアウト (`vitest` 側で設定可能だが、シミュレーション時間単位での上限は不可)

---

### 4. 高度な検証機能 [未実装・将来検討]

以下はプロフェッショナルな検証環境で期待される機能だが、現段階では設計スコープ外。将来の拡張候補として記録する。

| 機能 | 説明 |
|---|---|
| **制約付きランダム生成** | `$random` / SystemVerilog `randomize()` 相当 |
| **機能カバレッジ** | Covergroup / Coverpoint 相当の宣言的カバレッジ |
| **アサーションモニタ** | SVA (SystemVerilog Assertions) 相当の即時/並行アサーション |
| **トランザクションレベルモデリング** | Mailbox / Semaphore / FIFO プリミティブ |
| **BFM / ドライバ / モニタ** | プロトコル抽象化レイヤー |
| **Inout (Z 値ドライブ)** | TriState / High-Z の書き込み・読み取り |

---

### 5. コード内の既知 TODO

| ファイル | 内容 |
|---|---|
| `crates/celox-macros/src/generator.rs:119` | `// TODO: IO setter for arrays` — 配列ポートの IO setter 未実装 |
| `crates/celox/src/parser/ff.rs:23` | `// TODO: add clock` — FF 構造体にクロックフィールドなし |
| `crates/celox/src/parser/bitaccess.rs:7` | `// TODO: I feel this is definitely not enough` — ビットアクセス解析が不完全 |
| `crates/celox/src/parser/bitaccess.rs:17-20` | 定数畳み込みケースの TODO x2 |
| `crates/celox/src/parser/ff/expression.rs:277` | クロック不明時の一時的ハック |

---

## 推奨実装ロードマップ

### Phase 3a: 即時対応 (既存コードの接続・小規模変更)

1. **VCD 出力修正** — `NapiOptions` に `vcd` フィールド追加、builder に接続
2. **`next_event_time()` 公開** — NAPI メソッド追加のみ

### Phase 3b: テストベンチ体験の向上

3. **`waitUntil()` / イベント待ち API** — `step()` ベースのヘルパー層を TS 側で提供
4. **リセットヘルパー** — `sim.reset(cycles?)` のような便利メソッド
5. **タイムアウトガード** — `runUntil()` にオプションの `maxSteps` パラメータ

### Phase 3c: パワーユーザー機能

6. **`optimize` / `false_loop` / `true_loop` の NAPI 公開**
7. **パラメータオーバーライド**
8. **4-state マスク読み取りの高レベル API** — DUT getter から `FourStateValue` を返すオプション

### Phase 4: 高度な検証 (将来)

9. 内部信号アクセス / Force-Release
10. 信号監視コールバック
11. 非同期テストベンチフロー (`fork`/`join`)
12. 制約付きランダム / カバレッジ
