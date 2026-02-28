# TypeScript テストベンチ — 実装計画

> 前提: [ts_testbench_vision.md](./ts_testbench_vision.md) で定義した体験を実現するための技術的な実装計画。

## コアアイデア: SharedArrayBuffer による Zero-FFI I/O

シミュレータの内部メモリは単一の連続バッファであり、各信号はバイトオフセット + ビット幅でアクセスされる。このバッファを SharedArrayBuffer として JS 側に直接公開すれば、**信号の読み書きで NAPI 呼び出しが一切不要** になる。

Simulator (イベントベース) と Simulation (時間ベース) は同じメモリバッファを使うため、このアプローチは両方に共通。

```
    TypeScript (DUT Proxy)                      Rust (Cranelift JIT)
    ─────────────────────                       ────────────────────

    【イベントベース (Simulator)】
    dut.a = 100;
      └→ DataView.setUint32(offset, 100)    ──→  同じメモリを JIT が読む
    sim.tick()  ──── NAPI ──→                     tick() 実行
    dut.sum
      └→ DataView.getUint32(offset)         ←──  JIT が書いた結果を直接読む

    【時間ベース (Simulation)】
    dut.a = 100;
      └→ DataView.setUint32(offset, 100)    ──→  同じメモリ
    sim.runUntil(100)  ──── NAPI ──→              step() ループ実行
    dut.sum
      └→ DataView.getUint32(offset)         ←──  直接読む
```

**NAPI を通るのは制御操作だけ**:
- 共通: `create()`, `dump()`, `dispose()`
- イベントベース: `tick(eventId)`
- 時間ベース: `addClock()`, `schedule()`, `runUntil()`, `step()`
- 内部自動: `evalComb()` (出力読み取り時に必要に応じて透過的に呼ばれる)

**信号 I/O は DataView 操作のみ**: FFI オーバーヘッドゼロ

### メモリ共有の仕組み

```
SharedArrayBuffer (= Simulator の内部メモリを直接共有)
┌─────────────────────────────────────────────┐
│  STABLE 領域                                  │
│  [信号A: offset=0, 4bytes]                    │
│  [信号B: offset=4, 2bytes]                    │
│  [信号C(4state): offset=8, 4bytes value +     │
│                            4bytes mask]       │
│  ...                                          │
├─────────────────────────────────────────────┤
│  WORKING 領域 (JIT 内部用、JS はアクセスしない)  │
├─────────────────────────────────────────────┤
│  TRIGGERED BITS (JIT 内部用)                  │
└─────────────────────────────────────────────┘
```

- バッファはシミュレータ生存期間中リアロケーションされない
- リトルエンディアン、自然アライメント (1/2/4/8 byte 境界)
- JS 側は STABLE 領域のみ読み書きする

### 信号アクセスプロトコル

**イベントベース (Simulator)**:
```
1. create():    NAPI が SharedArrayBuffer + レイアウトマップを返す
2. dut.x = v:   JS が DataView で直接書き込み + dirty フラグ ON
3. sim.tick():   NAPI → FF 評価 + comb 再評価 → dirty クリア
4. dut.y:        dirty なら evalComb (NAPI) → DataView 読み取り
```

**時間ベース (Simulation)**:
```
1. create():              同上
2. sim.addClock("clk", period):  NAPI → スケジューラにクロック登録
3. dut.x = v:             JS が DataView で直接書き込み + dirty フラグ ON
4. sim.runUntil(100):     NAPI → step() ループ → dirty クリア
5. dut.y:                 dirty なら evalComb (NAPI) → DataView 読み取り
```

**組み合わせ回路のみのモジュール** (tick 不要):
```
1. dut.a = 1:   DataView 書き込み + dirty ON
2. dut.b = 2:   DataView 書き込み + dirty ON (変わらず)
3. dut.sum:     dirty → evalComb() 1回だけ NAPI 呼び出し → DataView 読み取り
4. dut.sum:     dirty = false → DataView 読み取りのみ (NAPI なし)
```

**Lazy evalComb**: 入力を書き込むだけでは evalComb は呼ばれない。出力を読む時に dirty なら自動的に1回だけ呼ばれる。`tick()` / `runUntil()` は内部で evalComb を呼ぶため dirty をクリアする。

安全性: JS からの書き込みは制御操作 (`tick` / `runUntil`) の前にのみ発生し、実行中は JS はブロックされる（同期 NAPI 呼び出し）。競合なし。

---

## 全体アーキテクチャ

```
┌─────────────────────────────────────────────────────────┐
│                   ユーザーの作業                          │
│  .veryl (HDL)  →  veryl gen-ts  →  test.ts (テストベンチ)│
└─────────────────────────────────────────────────────────┘
          │                                   │
          ▼                                   ▼
┌──────────────────┐               ┌──────────────────────┐
│  A. 型定義生成器   │               │  C. TS ランタイム層   │
│  (Rust CLI)       │               │  (TypeScript)        │
│                   │               │  - DUT Proxy         │
│  Analyzer IR      │               │    (SharedArrayBuffer │
│  → .d.ts / .js   │               │     + DataView)      │
└──────────────────┘               └──────────┬───────────┘
                                              │ NAPI は制御のみ
                                              ▼
                                   ┌──────────────────────┐
                                   │  B. NAPI バインディング │
                                   │  (Rust, napi-rs)      │
                                   │                       │
                                   │  create / tick / dump  │
                                   │  addClock / runUntil   │
                                   │  + SharedArrayBuffer   │
                                   └───────────────────────┘
```

---

## ワークストリーム分解

3 つの独立したストリームに分解でき、**A・B・C は大部分を並列に進められる**。

```
時間 →

Stream A (型定義生成)   ████ A1 ████ A2 ████ A3 ──┐
                                                    │
Stream B (NAPI binding) ████ B1 ████ B2 ████ B3 ──┤── 結合テスト (D)
                                                    │
Stream C (TS runtime)   ████ C1 ████ C2 ████ C3 ──┘
```

### 依存関係グラフ

```
A1 (型生成基盤)         B1 (NAPI 足場+SAB公開)     C1 (TS API設計)
    │                       │                          │
    ▼                       ▼                          ▼
A2 (interface/array)    B2 (Simulator + Simulation  C2 (DUT Proxy/DataView
    │                       制御API公開)                + Simulator/Simulation)
    ▼                       │                          │
A3 (CLI統合)                │                          ▼
    │                       │                      C3 (vitest統合)
    └───────────────────────┴──────────────────────────┘
                            │
                            ▼
                    D (結合テスト・E2E)
```

**並列実行のポイント**: A・B・C は合意したインターフェースを先に定義すれば独立して実装可能。結合は Stream D。

---

## Stream A: TypeScript 型定義生成器

既存の `simulator-macros` と同じデータソース (Analyzer IR) から TypeScript 型定義を生成する。

### A1: 基本的な型定義生成

**実装場所**: 新規モジュール `crates/simulator/src/ts_gen/` または独立クレート

**入力**: Analyzer IR — 既存の `simulator-macros` が使っている `Ir.components` と同じ
**出力**: モジュールごとの `.d.ts` + `.js`

**Veryl 型 → TypeScript 型のマッピング**:

| Veryl 型 | TS 型 | 理由 |
|----------|-------|------|
| `clock` | (DUT 非公開) | `sim.tick()` 経由で操作 |
| `reset` | `number` | 0 or 1 |
| `logic<1>`..`logic<53>` | `number` | JS 安全整数範囲内 |
| `logic<54>`.. | `bigint` | BigInt 必須 |
| `logic<W>[N]` | 配列型 (A2) | |
| interface port | ネストオブジェクト (A2) | |

**生成例**:

```veryl
module Adder (
    clk: input  clock,
    rst: input  reset,
    a:   input  logic<16>,
    b:   input  logic<16>,
    sum: output logic<17>,
) { ... }
```

↓

```typescript
// generated/Adder.d.ts
import type { ModuleDefinition } from "@veryl-lang/simulator";

export interface AdderPorts {
  rst: number;
  a: number;
  b: number;
  readonly sum: number;
}

export declare const Adder: ModuleDefinition<AdderPorts>;
```

```javascript
// generated/Adder.js
exports.Adder = {
  __veryl_module: true,
  name: "Adder",
  source: require("fs").readFileSync(__dirname + "/../Adder.veryl", "utf-8"),
  ports: {
    clk: { direction: "input", type: "clock" },
    rst: { direction: "input", type: "reset", width: 1 },
    a:   { direction: "input", type: "logic", width: 16 },
    b:   { direction: "input", type: "logic", width: 16 },
    sum: { direction: "output", type: "logic", width: 17 },
  },
  events: ["clk"],
};
```

`.js` にはポートのメタデータ (幅、方向) を含める。これを C の DUT Proxy が `create()` 時に消費し、DataView のオフセット計算に使う。

**テスト**: `simulator-macros` の既存テスト用 Veryl ソースを流用し、`insta` スナップショットで管理。

### A2: インターフェース・配列・wide 値

- **インターフェース**: ネストした型を生成 (`dut.bus.addr`)
- **配列**: `number[]` / `bigint[]` 型を生成 (`dut.data[0]`)
- **Wide 値** (≥54bit): `bigint` 型を生成

### A3: CLI 統合

`veryl gen-ts` サブコマンドを `crates/veryl/` に追加。既存の `veryl build` と同様のパイプライン。

---

## Stream B: NAPI バインディング

NAPI の役割は **SharedArrayBuffer の公開 + 制御操作のみ**。信号 I/O は一切通らない。

### B1: NAPI 足場 + SharedArrayBuffer 公開

**実装場所**: 新規クレート `crates/simulator-napi/`

**やること**:

1. napi-rs クレートの足場 (`cdylib`, `build.rs`, `package.json`)
2. `create()` で Simulator を構築し、内部メモリを SharedArrayBuffer として返す

**create の戻り値**:

```typescript
interface CreateResult {
  /** Simulator の内部メモリバッファを直接共有 */
  buffer: SharedArrayBuffer;

  /** 各信号のバッファ内位置情報 */
  layout: Record<string, SignalLayout>;

  /** ネイティブ制御ハンドル (tick 等に使う) */
  handle: NativeHandle;
}

interface SignalLayout {
  offset: number;      // STABLE 領域内のバイトオフセット
  width: number;       // ビット幅
  byteSize: number;    // 占有バイト数
  is4state: boolean;   // true なら value + mask で 2x byteSize
  direction: "input" | "output" | "inout";
}
```

**Rust 側の実装方針**:

- Simulator/Simulation 構築後、内部メモリバッファのポインタとサイズを取得
- napi-rs でゼロコピー共有 (バッファのライフタイムはシミュレータに紐づく)
- レイアウト情報は既存の MemoryLayout から各信号のオフセット・幅を読んで JS オブジェクトとして返す
- Simulation は Simulator を内包しているため、同じバッファを共有する

**EventRef の扱い**: イベント名 → 内部 ID のマップも `create()` 時に返す。`tick()` ではイベント名または ID を渡す。

### B2: 制御 API の公開 (Simulator + Simulation 同格)

**NAPI が公開するメソッド** (信号 I/O は含まない)。
Simulator と Simulation は同じ `create()` 戻り値構造を共有し、ハンドルの型だけが異なる。

**イベントベース (NativeSimulatorHandle)**:

```typescript
declare class NativeSimulatorHandle {
  tick(eventId: number): void;     // FF 評価 + evalComb
  evalComb(): void;                // DUT Proxy が内部で自動呼び出し (公開不要)
  dump(timestamp: number): void;
  dispose(): void;
}
```

**時間ベース (NativeSimulationHandle)**:

```typescript
declare class NativeSimulationHandle {
  addClock(eventId: number, period: number, initialDelay: number): void;
  schedule(eventId: number, time: number, value: number): void;
  runUntil(endTime: number): void; // 内部で step() ループ + evalComb
  step(): number | null;           // 次イベントまで進む + evalComb
  time(): number;
  evalComb(): void;                // DUT Proxy が内部で自動呼び出し (公開不要)
  dump(timestamp: number): void;
  dispose(): void;
}
```

get/set は JS 側の DataView 操作で完結するため、ハンドルには含まない。
`evalComb()` は NAPI 側に存在するが、ユーザーが直接呼ぶ必要はない — DUT Proxy が出力読み取り時に自動で呼ぶ。

**4-state の書き込み**: STABLE 領域の value 部分と mask 部分の両方を JS が DataView で書く。Rust 側の追加 API は不要。

**Simulation 内部の step() ロジック**: Simulation はスケジューラ・マルチフェーズ評価・カスケードクロック検出を内部で管理する。`runUntil()` や `step()` は Rust 側で完結し、JS はその結果を SharedArrayBuffer から読むだけ。

---

## Stream C: TypeScript ランタイム層

### C1: コア API 設計 (Simulator + Simulation 同格)

**実装場所**: `packages/simulator/`

**コア型** (`types.ts`):

```typescript
export interface ModuleDefinition<Ports = Record<string, unknown>> {
  __veryl_module: true;
  name: string;
  source: string;
  ports: Record<string, PortInfo>;
  events: string[];
}

export interface PortInfo {
  direction: "input" | "output" | "inout";
  type: "clock" | "reset" | "logic" | "bit";
  width: number;
  arrayDims?: number[];
  is4state?: boolean;
  interface?: Record<string, PortInfo>;
}
```

**Simulator** (イベントベース — 個々のクロックエッジを手動制御):

```typescript
export class Simulator<P> {
  static create<P>(module: ModuleDefinition<P>, options?: Options): Simulator<P>;
  get dut(): P;                          // DUT Proxy (SharedArrayBuffer ベース)
  tick(event?: EventHandle): void;       // クロックエッジ発火 (evalComb 自動)
  event(name: string): EventHandle;      // イベント名 → ハンドル
  dump(timestamp: number): void;
  dispose(): void;
}
```

**Simulation** (時間ベース — クロック周期を設定し時間を進める):

```typescript
export class Simulation<P> {
  static create<P>(module: ModuleDefinition<P>, options?: Options): Simulation<P>;
  get dut(): P;                          // 同じ DUT Proxy
  addClock(name: string, opts: { period: number; initialDelay?: number }): void;
  schedule(name: string, opts: { time: number; value: number }): void;
  runUntil(endTime: number): void;       // 指定時刻まで全イベント処理 (evalComb 自動)
  step(): number | null;                 // 次のイベントまで進む (evalComb 自動)
  time(): number;                        // 現在時刻
  dump(timestamp: number): void;
  dispose(): void;
}
```

**両クラスの共通点**: `dut` プロパティは同じ DUT Proxy (SharedArrayBuffer + DataView) を返す。信号 I/O の体験は完全に同一。違いは「時間をどう進めるか」だけ。

### C2: DUT Proxy (SharedArrayBuffer + DataView)

**核心**: `Proxy` + `DataView` + dirty フラグで、書き込みは FFI ゼロ、読み取りは必要時のみ evalComb。

```typescript
function createDutProxy<P>(
  buffer: SharedArrayBuffer,
  layout: Record<string, SignalLayout>,
  portDefs: Record<string, PortInfo>,
  handle: NativeHandle,  // evalComb 呼び出し用
): P {
  const view = new DataView(buffer);
  let dirty = false;

  // handle.tick / handle.runUntil のラッパーで dirty をクリア
  const markClean = () => { dirty = false; };

  return new Proxy({} as P, {
    get(_target, prop: string) {
      const sig = layout[prop];
      if (!sig) return undefined;

      // 出力ポートの読み取り時: dirty なら evalComb を自動実行
      if (dirty && portDefs[prop]?.direction !== "input") {
        handle.evalComb();
        dirty = false;
      }

      // ビット幅に応じた最適な読み取り (リトルエンディアン)
      if (sig.width <= 8)  return view.getUint8(sig.offset);
      if (sig.width <= 16) return view.getUint16(sig.offset, true);
      if (sig.width <= 32) return view.getUint32(sig.offset, true);
      if (sig.width <= 53) {
        const lo = view.getUint32(sig.offset, true);
        const hi = view.getUint32(sig.offset + 4, true) & ((1 << (sig.width - 32)) - 1);
        return lo + hi * 0x1_0000_0000;
      }
      return readBigInt(view, sig.offset, sig.byteSize);
    },

    set(_target, prop: string, value: number | bigint) {
      const sig = layout[prop];
      if (!sig) return false;
      if (portDefs[prop]?.direction === "output") {
        throw new Error(`Cannot write to output port '${prop}'`);
      }

      // DataView で直接書き込み (NAPI 呼び出しなし)
      if (sig.width <= 8)  { view.setUint8(sig.offset, Number(value)); }
      else if (sig.width <= 16) { view.setUint16(sig.offset, Number(value), true); }
      else if (sig.width <= 32) { view.setUint32(sig.offset, Number(value), true); }
      else { writeBigIntOrNumber(view, sig, value); }

      dirty = true;  // evalComb は出力読み取り時まで遅延
      return true;
    },
  });
}
```

**ポイント**:
- **書き込み**: 常に DataView のみ (FFI ゼロ)。dirty フラグを ON にするだけ
- **出力読み取り**: dirty なら evalComb を 1 回だけ NAPI で呼ぶ。以降は clean なので DataView のみ
- **入力読み取り**: evalComb 不要 (自分が書いた値をそのまま返す)
- **tick / runUntil**: 内部で evalComb を呼ぶため、戻り値で dirty をクリア

**インターフェースアクセス** (`dut.bus.addr`): ネストした Proxy を返す。`layout` のキーを `"bus.addr"` のようにドット区切りにするか、ネスト構造にするかは実装時に決定。

**配列アクセス** (`dut.data[0]`): 配列ポートは別の Proxy を返し、インデックスアクセスを要素幅分のオフセット計算に変換。

### C3: vitest / テストランナー統合

- vitest カスタムマッチャー (`toBeX()` 等)
- セットアップヘルパー (`beforeEach` で Simulator 作成、`afterEach` で dispose)
- 将来: vitest plugin で `.veryl` ファイルの自動 transform

---

## 並列作業マトリクス

| タスク | 依存先 | 規模 | 並列グループ |
|--------|--------|------|-------------|
| **A1** 基本型生成 | なし | M | **Group 1** |
| **B1** NAPI 足場 + SAB 公開 | なし | M | **Group 1** |
| **C1** TS API 設計 (Simulator + Simulation) | なし | S | **Group 1** |
| **A2** interface/array/wide | A1 | M | **Group 2** |
| **B2** 制御 API (Simulator + Simulation) | B1 | M | **Group 2** |
| **C2** DUT Proxy + Simulator/Simulation ラッパー | C1 | M | **Group 2** |
| **A3** CLI 統合 | A1 | S | **Group 2** |
| **C3** vitest 統合 | C2 | S | **Group 3** |
| **D1** 結合テスト | A1 + B2 + C2 | M | **Group 3** |
| **D2** E2E テスト | all | M | **Group 4** |

規模: S = 小, M = 中

### Group 1 (完全並列、依存なし)

```
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐
│ A1: 型生成基盤    │  │ B1: NAPI足場     │  │ C1: TS API設計       │
│                  │  │ + SharedArrayBuf  │  │ Simulator + Simulation│
│ Analyzer IR      │  │   公開            │  │ の型定義・クラス設計   │
│ → .d.ts/.js 生成 │  │                  │  │                      │
└──────────────────┘  └──────────────────┘  └──────────────────────┘
```

3 タスクとも外部依存なし。インターフェースの合意のみ先に行う。

### Group 2 (ストリーム内依存のみ、ストリーム間は並列)

```
┌────────────────┐  ┌──────────────────────┐  ┌──────────────────────────┐
│ A2: interface  │  │ B2: 制御API           │  │ C2: DUT Proxy            │
│     array/wide │  │  Simulator: tick/eval │  │     DataView + Proxy     │
│ A3: CLI統合    │  │  Simulation: addClock │  │  + Simulator ラッパー     │
│                │  │    runUntil/step/time │  │  + Simulation ラッパー    │
└────────────────┘  └──────────────────────┘  └──────────────────────────┘
```

B2 は Simulator と Simulation の制御メソッドを同時に実装する (内部メモリ共有は B1 で済んでいるため、薄いラッパー)。
C2 は B に依存しない — SharedArrayBuffer の形式だけ合意していれば独立実装可能。

### Group 3-4

```
C3 (vitest) + D1 (結合テスト) → D2 (E2E)
```

---

## ストリーム間の合意事項

並列実装のために最初に固定するインターフェース:

### 1. NAPI の create() が返す構造

```typescript
// B が返し、C が消費する
interface CreateResult {
  buffer: SharedArrayBuffer;
  layout: Record<string, SignalLayout>;
  events: Record<string, number>;  // イベント名 → ID
  handle: NativeHandle;
}

interface SignalLayout {
  offset: number;
  width: number;
  byteSize: number;
  is4state: boolean;
  direction: "input" | "output" | "inout";
}
```

### 2. NativeHandle のメソッド

```typescript
// B が実装し、C が呼ぶ
// evalComb は両方に存在するが、DUT Proxy が内部で自動呼び出しするため
// ユーザー公開 API には含めない

// イベントベース
interface NativeSimulatorHandle {
  tick(eventId: number): void;
  evalComb(): void;       // Proxy 内部用
  dump(timestamp: number): void;
  dispose(): void;
}

// 時間ベース
interface NativeSimulationHandle {
  addClock(eventId: number, period: number, initialDelay: number): void;
  schedule(eventId: number, time: number, value: number): void;
  runUntil(endTime: number): void;
  step(): number | null;
  time(): number;
  evalComb(): void;       // Proxy 内部用
  dump(timestamp: number): void;
  dispose(): void;
}
```

### 3. ModuleDefinition (型生成器の出力形式)

```typescript
// A が生成し、C が消費する
interface ModuleDefinition<Ports> {
  __veryl_module: true;
  name: string;
  source: string;
  ports: Record<string, PortInfo>;
  events: string[];
}

interface PortInfo {
  direction: "input" | "output" | "inout";
  type: "clock" | "reset" | "logic" | "bit";
  width: number;
  arrayDims?: number[];
  is4state?: boolean;
  interface?: Record<string, PortInfo>;
}
```

---

## ファイル配置

```
veryl/
├── crates/
│   ├── simulator/              # 既存 (メモリバッファ公開の pub メソッド追加)
│   ├── simulator-macros/       # 既存 (変更なし)
│   ├── simulator-napi/         # 【新規】Stream B
│   │   ├── Cargo.toml          #   crate-type = ["cdylib"]
│   │   ├── build.rs
│   │   ├── package.json
│   │   └── src/
│   │       ├── lib.rs          #   create, tick, eval, dump, dispose
│   │       └── layout.rs       #   MemoryLayout → JS SignalLayout 変換
│   ├── simulator-ts-gen/       # 【新規】Stream A
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── generator.rs    #   IR → .d.ts/.js 生成
│   └── veryl/                  # 既存 CLI (A3 で gen-ts サブコマンド追加)
└── packages/
    └── simulator/              # 【新規】Stream C — @veryl-lang/simulator
        ├── package.json
        ├── tsconfig.json
        └── src/
            ├── index.ts
            ├── simulator.ts    #   Simulator クラス (イベントベース)
            ├── simulation.ts   #   Simulation クラス (時間ベース)
            ├── proxy.ts        #   DUT Proxy (SharedArrayBuffer + DataView)
            ├── types.ts        #   ModuleDefinition, PortInfo 等
            └── matchers.ts     #   vitest カスタムマッチャー
```

---

## simulator クレートへの変更

SharedArrayBuffer のために、既存の `simulator` クレートに最小限の pub メソッドを追加する必要がある:

1. **メモリバッファのポインタ + サイズ取得**: JIT バックエンドの内部バッファへのアクセス
2. **レイアウト情報の取得**: 各信号の `(offset, width, is_4state)` 一覧
3. **イベント ID → EventRef マッピング**: イベント名から内部 ID への解決

これらは read-only なアクセサであり、既存の動作に影響しない。

---

## リスクと対策

| リスク | 対策 |
|--------|------|
| SharedArrayBuffer のブラウザ制約 (COOP/COEP) | Node.js/Bun では制約なし。ブラウザターゲットは Phase 1 のスコープ外 |
| メモリバッファのリアロケーション | シミュレータは構築後にバッファサイズが変わらないことを確認済み |
| ビット幅の端数処理 | Rust 側が読み取り時にマスクする仕様に合わせ、JS 側は書き込み時にマスク |
| Proxy のオーバーヘッド | ホットパスでは `layout` から取得した offset で直接 DataView 操作する低レベル API も提供 |
| BigInt 変換コスト (wide 値) | 53bit 以下は `number` 高速パス。wide は DataView → BigInt の直接変換 |
| Bun の NAPI 互換性 | 早期にスモークテストを CI に組み込む。SharedArrayBuffer は Node-API の基本機能であり互換性は高い |
