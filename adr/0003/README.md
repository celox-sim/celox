# ADR-0003: Self-Hosted x86-64 Backend

- **Date**: 2026-03-24
- **Status**: implemented
- **Updated**: 2026-03-28

## Context

Celox は Cranelift を JIT バックエンドとして使用していたが、以下の問題により限界に達していた。

### Cranelift / regalloc2 の問題

1. **Live register 数のスケーリング破綻**: regalloc2 の backtracking allocator は、同時に live な register が数百〜数千になるとコンパイル時間が爆発する。wasmtime issue #3523 では、大きな関数のコンパイル時間の 96% が regalloc に消費された（15.48s / 16.04s）
2. **VReg 上限**: regalloc2 は Operand を 32bit に pack しており、VReg 数が 2^21（約 210 万）に制限される。64bit 化は 5-10% の性能劣化を伴う
3. **メモリ割り当てオーバーヘッド**: `try_to_allocate_bundle_to_reg` 単体で 1600 万回の一時 HashSet 確保が発生（issue #87）
4. **大きい Execution Unit への対応不能**: HDL シミュレーションの組み合わせ回路は巨大な関数を生成するが、Cranelift の主要ユーザーである wasmtime は小さな WASM 関数が前提

### 以前の workaround（Cranelift 時代）

- **Tail-call splitting**: EU を CLIF 命令閾値・VReg 閾値以下に分割
- **split_wide_commits**: Cranelift が wide value を扱えないため SIRT 側で事前分割
- **reschedule**: Cranelift の regalloc のためのプレッシャー制御

## Decision

Cranelift を自前の x86-64 コードジェネレータで完全に置き換えた。デフォルトバックエンドとして切り替え済み。

## Pipeline Architecture（実装済み）

```
SIR (bit-level SSA)
  ↓ ISel (isel.rs)
MIR (word-level SSA, virtual registers) + SpillDesc per VReg
  ↓ Analysis (analysis.rs) — liveness, next-use distance
  ↓ Unified single-pass allocator (unified.rs)
    — simultaneous spilling + assignment using RegFile
MIR (physical registers)
  ↓ Emit (emit.rs) — iced-x86 CodeAssembler
x86-64 machine code
```

### MIR Design

MIR は SIR（bit-level）と machine code の間に位置する word-level SSA IR。

- **64-bit レジスタ単位**: 全操作は 64-bit VReg 上で行う。>64-bit 値はチャンク配列
- **SpillDesc サイドテーブル**: 各 VReg のスピルコスト情報。bit-level のアクセス情報を保持し、regalloc 本体は bit layout を知らなくてよい

```
SpillCost:
  Rematerializable  → cost 0   // 即値、再生成するだけ
  SimStateAligned   → cost 1   // static offset, word-aligned
  SimStateSmall     → cost 2   // static offset, width < native width
  SimStateDynamic   → cost 4   // dynamic offset
  Transient         → cost 2   // スタック spill slot
```

### ISel (isel.rs)

SIR → MIR 変換。以下を処理:

- **Narrow (≤64-bit)**: Load/Store, 全算術・論理・比較・シフト演算、Concat, Slice
- **Wide (>64-bit)**: チャンク分割による Add/Sub（carry chain）, Mul（schoolbook O(n²)）, Div/Rem（bit-by-bit restoring）, Shl/Shr/Sar（定数: チャンクシフト+キャリー、ランタイム: select chain）, 比較（MSB-to-LSB cascading）, Concat repack, Slice
- **4-state**: 全操作に対するマスク伝播。値の正規化（`v |= m`、X = v:1,m:1）
  - AND/OR/XOR: per-bit mask formula
  - Arithmetic/Comparison: conservative（any X → all-X）
  - Shift: 定数シフトはマスクも同様にシフト、ランタイムシフト amount X → all-X
  - LogicAnd/Or: IEEE 1800 dominant-value semantics
  - Reduction AND/OR: IEEE 1800 dominant-value semantics（definite-0/1 → 結果確定）
  - Wildcard ==?/!=?: RHS X/Z ビットは don't-care
  - Concat: narrow/wide ともマスクチャンクの repack
  - Store to bit type: ソースレジスタのマスクを 0 にクリア

### Unified Register Allocator (unified.rs)

当初は Braun & Hack の separate-pass 設計（spilling → assignment）を実装したが、spilling と assignment の live set tracking に構造的な差分が生じ、k-1 ハックや eviction fallback が必要になった。最終的に **unified single-pass allocator** に置き換えた。

#### RegFile

```rust
struct RegFile {
    preg_to_vreg: BTreeMap<PhysReg, VReg>,
    vreg_to_preg: BTreeMap<VReg, PhysReg>,
}
```

物理レジスタ ↔ VReg の双方向マップで、各プログラムポイントの正確なレジスタ状態を追跡。

#### アルゴリズム

1. **Entry state**: predecessor の exit RegFile をマージ。phi coalescing を試行
2. **Forward walk**: 各命令で uses を ensure（reload if needed）→ evict to k → assign def
3. **Eviction**: next-use distance 最遠 + remat/store-back-only を優先的に evict
4. **Fixed constraints**: shift rhs → RCX。occupant を spill + Mov で退避。pinned VReg 追跡
5. **Clobbers**: UDiv/URem の RAX/RDX clobber。blocked set でこれらを def 割り当てから排除
6. **Coupling code**: predecessor exit ↔ successor entry の差分で spill/reload を挿入

k = NUM_REGS = 13 (x86-64: 16 GPR - RSP - RBP - SimState base)。k-1 ハック不要。

#### next-use lookup

`use_positions: BTreeMap<VReg, Vec<usize>>` による O(log n) binary search。O(n) forward scan を置き換え。

### Verifier (regalloc.rs)

`#[cfg(debug_assertions)]` で有効。同時 live な VReg が同一 PhysReg を共有しないことを検証。use_positions テーブルによる O(n log n) dead check（O(n²) verifier を修正、sorter N=64 が 180s timeout → 5.3s に改善）。

### Code Emission (emit.rs)

iced-x86 CodeAssembler で x86-64 機械語を生成。

- Prologue/Epilogue: callee-saved push/pop, frame allocation, SimState base (R15) setup
- Multi-EU chaining: 各 EU の ret を jmp rel32 にパッチして連鎖
- Select emit: `dst == true_val` 時の reverse logic（cmove）
- UMulHi: `mul r64` → RDX 取得（wide Mul の上位 64 bit）

## 4-State Support

IEEE 1800 の 4-state logic をネイティブサポート。

### Memory Layout

`MemoryLayout::build(sir, four_state=true)` で全変数に value + mask の二重領域を割り当て。mask は value の直後（`offset + byte_size`）。

### Encoding

| mask | value | 意味 |
|------|-------|------|
| 0 | 0 | 0（definite zero）|
| 0 | 1 | 1（definite one）|
| 1 | 0 | Z（high-impedance）|
| 1 | 1 | X（unknown）|

### 値の正規化

Binary 演算後に `value |= mask` を適用。X ポジション（mask=1）の値ビットを必ず 1 にし、エンコーディングの一貫性を保証。

### 初期化

`logic` 型変数は X で初期化（value=0xFF, mask=0xFF）。`bit` 型は 0 で初期化。

## Complexity

| Phase | 計算量 |
|---|---|
| Analysis (liveness, next-use distance) | O(n) per iteration, bounded |
| ISel | O(n) |
| Unified allocator (spill + assign) | O(n × k) |
| Verifier (debug builds) | O(n × k × log n) |
| Code emission | O(n) |
| **Total** | **O(n × k)** ≈ **O(n)** |

### Benchmark: SorterTreeDistEntry

| N | 旧 (Cranelift workaround 時代) | 新 (native backend) |
|---|---|---|
| 16 | ~24 min (branch-based mux) | 1.0s |
| 64 | timeout | 5.3s |
| 128 | — | 15.1s |

## 削減・簡素化されたパス

| パス | Cranelift 時代 | Native backend 後 |
|---|---|---|
| `tail_call_split` | regalloc2 の VReg 上限回避 | **不要** |
| `split_wide_commits` | Cranelift が wide value を扱えない | **不要**（ISel がチャンク処理）|
| `reschedule` | regalloc のためのプレッシャー制御 | **簡素化可能** |

## Target Architecture Strategy

MIR はターゲット中立。emit phase のみターゲット別に分岐。

```
SIR (bit-level, 共通)
  ├── native backend: SIR → ISel → MIR → Unified Alloc → Emit (x86-64)
  └── wasm backend: SIR → WASM codegen（MIR を経由しない）
```

AArch64 / RISC-V 対応は emit phase の追加で可能（k, ABI, 命令エンコーディングが異なる）。

## References

- Braun, M. and Hack, S. (2009). "Register Spilling and Live-Range Splitting for SSA-Form Programs." CC 2009.
- Hack, S., Grund, D., and Goos, G. (2006). "Register Allocation for Programs in SSA-Form." CC 2006.
- Schwarz, T., Kamm, T., and Engelke, A. (2025). "TPDE: A Fast Adaptable Compiler Back-End Framework." CGO 2026.
- Cranelift/regalloc2 issues: wasmtime#3523, wasmtime#8783, regalloc2#87
