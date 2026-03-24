# ADR-0003: Custom Register Allocator for Self-Hosted Backend

- **Date**: 2026-03-24
- **Status**: proposed

## Context

Celox は現在 Cranelift を JIT バックエンドとして使用しているが、以下の問題により限界に達している。

### Cranelift / regalloc2 の問題

1. **Live register 数のスケーリング破綻**: regalloc2 の backtracking allocator は、同時に live な register が数百〜数千になるとコンパイル時間が爆発する。wasmtime issue #3523 では、大きな関数のコンパイル時間の 96% が regalloc に消費された（15.48s / 16.04s）
2. **VReg 上限**: regalloc2 は Operand を 32bit に pack しており、VReg 数が 2^21（約 210 万）に制限される。64bit 化は 5-10% の性能劣化を伴う
3. **メモリ割り当てオーバーヘッド**: `try_to_allocate_bundle_to_reg` 単体で 1600 万回の一時 HashSet 確保が発生（issue #87）
4. **大きい Execution Unit への対応不能**: HDL シミュレーションの組み合わせ回路は巨大な関数を生成するが、Cranelift の主要ユーザーである wasmtime は小さな WASM 関数が前提

### 現状の workaround

- **Tail-call splitting** (`pass_tail_call_split.rs`): EU を CLIF 命令閾値（8M）・VReg 閾値（100 万）以下に分割し、tail-call で連鎖。境界で live register を scratch memory に spill するオーバーヘッドが発生
- **split_wide_commits**: Cranelift が wide value を扱えないため SIRT 側で事前分割
- **reschedule**: Cranelift の regalloc が最適でないため、SIRT 側でレジスタプレッシャーを意識したスケジューリングを実施
- **bit_extract_peephole** 等: bit-level ISA → word-level ISA 変換を SIRT 最適化パスで実施（本来は codegen の責務だがフロントエンドの問題でもあり現状維持）

これらの workaround は Cranelift の制約を回避するために SIRT に不自然な変換を持ち込んでおり、保守コストが高い。

## Decision

Cranelift を自前のコードジェネレータで置き換える。レジスタ割り当てアルゴリズムとして、Braun & Hack (2009) の SSA-based spilling を HDL シミュレーション向けに拡張した方式を採用する。

## Pipeline Architecture

```
SIR (bit-level SSA)
  ↓ ISel
MIR (word-level SSA, virtual registers) + SpillDesc per vreg
  ↓ Spilling (Braun & Hack extended MIN)
MIR (pressure ≤ k, spill/reload inserted)
  ↓ Assignment (greedy coloring on chordal graph)
MIR (physical registers)
  ↓ Emit
x86-64 machine code
```

### MIR Design

MIR は SIR（bit-level）と machine code の間に位置する word-level SSA IR。bit-level のアクセス情報は `SpillDesc` サイドテーブルにのみ保持し、regalloc 本体は bit layout を知らなくてよい。

```rust
// SpillDesc: 各 VReg のスピルコスト情報
SpillCost:
  Rematerializable  → cost 0   // SIRValue（即値）、再生成するだけ
  SimStateAligned   → cost 1   // static offset, word-aligned
  SimStateSmall     → cost 2   // static offset, width < native width（mask 操作必要）
  SimStateDynamic   → cost 4   // dynamic offset
  Transient         → cost 2   // スタック spill slot
```

## Algorithm Design

### Overview

Braun & Hack の MIN アルゴリズム（Belady のページ置換アルゴリズムの CFG 一般化）を基盤とし、spilling phase と assignment phase を分離する。

- **Spilling phase**: レジスタプレッシャーを物理レジスタ数 k 以下に削減するプログラム変換
- **Assignment phase**: SSA-form プログラムの interference graph は chordal であるため、線形時間で最適な割り当てが可能（Hack et al. 2006）

この分離により、domain-specific な知識を spilling phase に集中して注入できる。

### Phase 0: Analysis（スケジューラと共有）

既存のスケジューラが計算する情報をそのまま使用：

- **Liveness**: 各 SIR register の live range（定義点〜最終使用点）
- **Global next-use distance**: Braun & Hack Section 4.1 に準拠。CFG 上の data-flow 解析。ループ脱出辺には length M（大きい定数）を付与し、ループ内の use を優先
- **Register pressure profile**: 各プログラムポイントでの同時 live register 数

### Phase 1: Spill Cost Classification

各 SIR register に対して、simulation state 上の物理的な居場所から spill/reload コストを算出する。SIR 命令の addr/offset/width から直接分類可能。ISel 段階で SpillDesc として MIR の VReg に紐付ける。

#### store-back only 判定

SIR register が定義後、Store にのみ使用される場合、その値は Store 時点で simulation state に書き込まれるため spill store が不要（既にメモリにある）。eviction 時の spill store を省略できる。

#### Rematerialization

`SIRValue(即値)` は spill せず再生成する。spill store も reload も不要で、定数一つの mov 命令で復元できる。

### Phase 2: Spilling（Braun & Hack 拡張 MIN）

CFG を reverse post order で走査し、各基本ブロックに MIN アルゴリズムを適用する。

#### Eviction 優先度

オリジナルの MIN: next-use distance が最も遠い値を evict。

Celox 拡張:

```
eviction_priority(v, insn) =
  if is_rematerializable(v):
    ∞                                          // 最優先で evict（再生成 cost 0）
  elif is_store_back_only(v) && spill_cost(v) <= SimStateAligned:
    ∞                                          // spill store 不要、reload も安い
  else:
    next_use_distance(v, insn) / reload_cost(v)
```

priority が高いほど先に evict される。

#### W^entry（ブロック入口レジスタ集合）の初期化

Braun & Hack Section 4.2 に準拠:

- **通常ブロック**: predecessor の W^exit の共通部分 + next-use distance と reload cost 重み付きでソートし、残りスロットを埋める
- **ループヘッダ**: ループ内で使用される値を優先。reload cost が高い値はより優先的に W^entry に入れ、ループ内での再 load を避ける

#### Coupling code（ブロック間の接続）

Braun & Hack Section 4.3 に準拠。predecessor の W^exit と successor の W^entry の差分に基づき、制御辺上に spill/reload コードを挿入する。

#### SSA 保持

Braun & Hack Section 4.4 に準拠。reload の挿入で壊れた SSA を φ-function 挿入により再構築する。

### Phase 3: Assignment

Spilling 後、各プログラムポイントの同時 live register 数が k 以下であることが保証される。SSA-form の interference graph は chordal であるため、greedy coloring で最適解が O(n) で得られる。

### Phase 4: Code Emission

SIR 命令 → x86-64 命令列の直接 emit。x86-64 のエンコーディングには既存クレート（`iced-x86` 等）の利用を検討する。

#### Concat 分解

Concat + wide Store パターンは codegen が Concat を「論理的な操作」として扱い、チャンクごとに順次 Store を emit する。これにより register pressure のピークを大幅に削減できる。

## Commit とメモリバリア

Commit は `src region → dst region` のメモリコピーであり、同一 addr の simulation state の値を変更する。regalloc は Commit を **メモリバリアとして扱う必要がある**：simulation state を spill 先とする値の reload を Commit 越しに移動してはならない（Commit 前後で state の値が変わるため）。スタック spill（Transient）は Commit の影響を受けない。

SIRT 最適化パス（commit_sinking, inline_commit_forwarding, hoist_common_branch_loads）が Commit 配置を最適化済みだが、regalloc はこの制約を守る必要がある。

spill cost への影響:

```
spill_cost(v, program_point) =
  if commit_on_same_addr_between(v.definition, program_point):
    Transient (cost 2)  // simulation state reload 不可、スタック使用
  else:
    通常の分類
```

## FF vs Comb の性質の違い

同一のアルゴリズムで処理するが、性質が異なる。

### FF (eval_apply_ffs / eval_only_ffs)

- **CFG が複雑**: branch, merge block あり。Braun & Hack の CFG 対応（global next-use distance, W^entry 初期化, coupling code）の真価が出る局面
- **Commit 操作あり**: メモリバリア制約が適用される
- **Register pressure は比較的低い**

### Comb (eval_comb)

- **CFG は単純**: 巨大な直線コード or 浅い if-else
- **Commit なし**: Load → 計算 → Store の直線的な流れ
- **Register pressure が爆発する本丸**: 大きい組み合わせ回路で数百〜数千の signal が同時 live
- Rematerialization と store-back only 判定の効果が最大化される局面

## 削減・簡素化されるパス

| パス | 現状の役割 | 自前 backend 後 |
|---|---|---|
| `tail_call_split` | regalloc2 の VReg 上限・スケーリング回避 | **不要**（線形スケーリング） |
| `split_wide_commits` | Cranelift が wide value を扱えない問題の回避 | **不要**（codegen がチャンク処理） |
| `reschedule` | Cranelift regalloc のためのプレッシャー制御 | **簡素化**（spill phase がプレッシャー制御。メモリ並列性のみ最適化） |
| `bit_extract_peephole` | bit-level → word-level 変換 | **維持**（フロントエンド改善まで必要） |
| `store_load_forwarding` | IR 上の冗長 load 除去 | **維持** |
| `commit_sinking` | commit 配置最適化 | **維持** |
| `inline_commit_forwarding` | commit のインライン化 | **維持** |
| `eliminate_dead_working_stores` | 意味論レベル DCE | **維持** |
| `coalesce_stores` | 隣接 store の結合 | **維持** |
| `optimize_blocks` | dead block 除去等 | **維持** |

## Complexity Analysis

| Phase | 計算量 |
|---|---|
| Analysis (liveness, next-use distance) | O(n) per data-flow iteration, bounded iterations |
| Spill cost classification | O(n) — SIR 命令を 1 回走査 |
| Spilling (MIN on CFG) | O(n × k) — n = 命令数, k = 物理レジスタ数 |
| Assignment (greedy coloring) | O(n) — SSA chordal graph |
| Code emission | O(n) |
| **Total** | **O(n × k)** ≈ **O(n)** (k は定数: x86-64 GPR 数) |

regalloc2 がパス全体で superlinear になる問題を根本的に回避する。

## Target Architecture Strategy

MIR はターゲット中立な word-level SSA であり、regalloc（spilling + assignment）まで共通で動作する。ターゲット別に分岐するのは emit phase のみ。

```
SIR (bit-level, 共通)
  ├── native backend: SIR → ISel → MIR → Spilling → Assignment → Emit
  │     └── Emit: x86-64 / AArch64 / RISC-V
  └── wasm backend: SIR → WASM codegen（既存、MIR を経由しない）
```

ターゲット間で異なるパラメータ：
- **k**（物理レジスタ数）: x86-64 = 13-14 GPR, AArch64 = 28, RISC-V = 28
- **ABI**: simulation state base pointer をどのレジスタに置くか
- **Emit**: 命令エンコーディング（既存クレートの利用を検討）

WASM はスタックマシンであり regalloc が不要なため、既存の `wasm_codegen.rs` による SIR → WASM 直接変換を維持する。

## References

- Braun, M. and Hack, S. (2009). "Register Spilling and Live-Range Splitting for SSA-Form Programs." CC 2009. — spilling phase の基盤
- Hack, S., Grund, D., and Goos, G. (2006). "Register Allocation for Programs in SSA-Form." CC 2006. — SSA interference graph の chordality 証明、線形時間 assignment
- Schwarz, T., Kamm, T., and Engelke, A. (2025). "TPDE: A Fast Adaptable Compiler Back-End Framework." CGO 2026. — single-pass backend 設計、snippet encoder の参考
- Pereira, F. and Palsberg, J. (2008). "Register Allocation by Puzzle Solving." PLDI 2008. — SSA 上の O(n) allocation（参考）
- Cranelift/regalloc2 issues: wasmtime#3523, wasmtime#8783, regalloc2#87 — 現行 backend の問題の裏付け
