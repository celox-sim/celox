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

### ~~1. VCD 波形出力が TS から設定不可~~ [修正済み]

`NapiOptions` に `vcd: Option<String>` を追加し、全コンストラクタ/ファクトリで `builder.vcd(path)` に接続。TS 側の `fromSource` / `fromProject` / ブリッジ関数からも `vcd` オプションが NAPI に渡されるようになった。

---

### 2. Rust に実装済みだが TS に未公開の機能

| 機能 | Rust API | NAPI | TS | 備考 |
|---|---|---|---|---|
| ~~**`optimize` フラグ**~~ | `builder.optimize(bool)` | **公開済み** | **公開済み** | `SimulatorOptions.optimize` で設定可能 |
| **`TraceOptions`** | `builder.trace(opts)` + 11 個の個別メソッド | 未公開 | 未公開 | デバッグ/プロファイリング用。CLIF IR、ネイティブ ASM 等 |
| **`build_with_trace()`** | `builder.build_with_trace()` | 未公開 | 未公開 | コンパイルトレース結果を取得 |
| ~~**`false_loop(from, to)`**~~ | `builder.false_loop(...)` | **公開済み** | **公開済み** | `SimulatorOptions.falseLoops` で設定可能 |
| ~~**`true_loop(from, to, max_iter)`**~~ | `builder.true_loop(...)` | **公開済み** | **公開済み** | `SimulatorOptions.trueLoops` で設定可能 |
| ~~**`next_event_time()`**~~ | `Simulation::next_event_time()` | **公開済み** | **公開済み** | `sim.nextEventTime()` で利用可能 |
| ~~**`get_four_state(signal)`**~~ | `Simulator::get_four_state()` | **公開済み** | **公開済み** | `sim.fourState(portName)` で `FourStateValue` を取得可能 |

#### 優先度判定

- ~~**高**: `next_event_time()` — テストベンチでの時間制御に有用~~ (実装済み)
- ~~**中**: `optimize`, `false_loop`, `true_loop` — パワーユーザー向け~~ (実装済み)
- **低**: `TraceOptions` / `build_with_trace` — 開発者デバッグ用

---

### 3. テストベンチとして不足している主要機能

#### ~~3.1 イベント待ち / 条件待ちヘルパー~~ [実装済み]

`waitUntil()` と `waitForCycles()` を `Simulation` クラスに追加:

```typescript
// 条件待ち (デフォルト maxSteps: 100,000)
const t = sim.waitUntil(() => dut.done === 1);

// クロックサイクル待ち (1 cycle = 2 steps)
const t = sim.waitForCycles("clk", 10);
```

`SimulationTimeoutError` で無限ループを防止。残りの高度な非同期機能は Phase 4:

- `@(posedge clk)` 相当の待ち受け
- コールバックベースのイベント登録
- `fork` / `join` による並行刺激
- async generator ベースのテストベンチフロー

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

#### ~~3.6 リセットヘルパー~~ [実装済み]

`Simulation.reset()` メソッドを追加:

```typescript
// activeValue (デフォルト 1) を書き込み → activeCycles (デフォルト 2) 進行 → 0 に戻す
sim.reset("rst");
sim.reset("rst", { activeCycles: 3, activeValue: 1 });
```

#### ~~3.7 タイムアウト / シミュレーション安全ガード~~ [実装済み]

`SimulationTimeoutError` クラスと `maxSteps` ガードを追加:

- `runUntil(endTime, { maxSteps })` — maxSteps 未指定時は高速 Rust パスを使用
- `waitUntil(condition, { maxSteps })` — デフォルト 100,000 ステップ
- `waitForCycles(event, count, { maxSteps })` — デフォルト 100,000 ステップ
- `SimulationTimeoutError` に `time` と `steps` プロパティ

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

### ~~Phase 3a: 即時対応 (既存コードの接続・小規模変更)~~ [完了]

1. ~~**VCD 出力修正** — `NapiOptions` に `vcd` フィールド追加、builder に接続~~ ✅
2. ~~**`next_event_time()` 公開** — NAPI メソッド追加のみ~~ ✅

### ~~Phase 3b: テストベンチ体験の向上~~ [完了]

3. ~~**`waitUntil()` / `waitForCycles()` API** — `step()` ベースのヘルパー層を TS 側で提供~~ ✅
4. ~~**リセットヘルパー** — `sim.reset(signal, opts?)` メソッド~~ ✅
5. ~~**タイムアウトガード** — `runUntil()` に `maxSteps`、`SimulationTimeoutError`~~ ✅

### ~~Phase 3c: パワーユーザー機能~~ [完了]

6. ~~**`optimize` / `false_loop` / `true_loop` の NAPI 公開**~~ ✅
7. **パラメータオーバーライド** [未実装]
8. ~~**4-state マスク読み取りの高レベル API** — `sim.fourState(portName)` で `FourStateValue` を返す~~ ✅

### Phase 4: 高度な検証 (将来)

9. パラメータオーバーライド (Phase 3c から移動)
10. 内部信号アクセス / Force-Release
11. 信号監視コールバック
12. 非同期テストベンチフロー (`fork`/`join`)
13. 制約付きランダム / カバレッジ
