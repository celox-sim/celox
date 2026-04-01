# 最適化チューニング

Celox には **SIRT パス**（Celox 独自の IR オプティマイザ）と **Cranelift バックエンドオプション** の 2 層の最適化制御があります。すべての最適化はデフォルトで有効です。このガイドではワークロードに応じたチューニング方法を説明します。

::: tip TL;DR
デフォルト設定（全有効）が最も汎用的です。コンパイル時間やシミュレーション速度に具体的なボトルネックがある場合のみチューニングし、必ず実際のデザインでベンチマークしてください。
:::

## クイックスタート

```ts
import { Simulator } from '@celox-sim/celox';

// デフォルト: 全最適化有効（最速のシミュレーション速度）
const sim = await Simulator.create(module);

// 高速コンパイルモード（シミュレーションは大幅に遅くなる）
const sim = await Simulator.create(module, {
    craneliftOptLevel: "none",
    regallocAlgorithm: "singlePass",
    enableAliasAnalysis: false,
    enableVerifier: false,
});

// 全 SIRT パスを無効化（Cranelift の最適化は有効のまま）
const sim = await Simulator.create(module, { optimize: false });
```

## SIRT 最適化パス

SIRT（Simulator IR Transform）パスは、Cranelift にコード生成を渡す前に中間表現を最適化します。

| パス | 動作 |
|---|---|
| `storeLoadForwarding` | Store した値を再ロードせずに直接再利用する |
| `hoistCommonBranchLoads` | 条件分岐の両方が同じ Load で始まる場合、分岐前に巻き上げる |
| `bitExtractPeephole` | `(value >> shift) & mask` を単一のレンジロードに変換 |
| `optimizeBlocks` | デッドブロック除去、ブロックマージ |
| `splitWideCommits` | 幅広いコミット操作を狭い操作に分割 |
| `commitSinking` | コミット操作を使用箇所の近くに移動 |
| `inlineCommitForwarding` | 中間コピーを排除し、宛先リージョンに直接書き込む |
| `eliminateDeadWorkingStores` | 読まれないワーキングメモリへの Store を除去 |
| `reschedule` | Cranelift のコード生成に有利な命令順序に並べ替え |
| `coalesceStores` | 連続する狭い Store を幅広い Concat+Store にマージ |

### パス間の相互作用

各パスは**独立ではなく**、パイプラインとして機能します。前段のパスが後段の最適化を可能にします:

```
storeLoadForwarding ─┐
                     ├─► きれいな IR ──► commitSinking ──► inlineCommitForwarding ──► ...
hoistCommonBranchLoads┘
```

`storeLoadForwarding` と `hoistCommonBranchLoads` が IR を簡素化し、`inlineCommitForwarding` がコミットパターンを効率的にマッチできるようにします。個別に無効化すると無害に見える場合でも、**組み合わせて無効化すると** Cranelift に渡される IR の品質が低下し、コンパイル時間とシミュレーション速度が悪化します。

::: warning
`storeLoadForwarding`、`hoistCommonBranchLoads`、`inlineCommitForwarding` をまとめて無効化しないでください。ベンチマークでは、この組み合わせにより組み合わせ回路のコンパイル時間が +69%、eval 時間が +17% 増加しました。
:::

### クリティカルパス

シミュレーション速度への影響が大きいパスです。無効化すると大幅な性能低下を引き起こします:

| パス | 順序回路 (tick) | 組み合わせ回路 (eval) |
|---|---|---|
| `reschedule` | **+322%** 低下 | +9% 低下 |
| `commitSinking` | **+207%** 低下 | +14% 低下 |
| `eliminateDeadWorkingStores` | **+163%** 低下 | +9% 低下 |
| `splitWideCommits` | **+161%** 低下 | +11% 低下 |
| `optimizeBlocks` | ほぼ中立 | **+71%** 低下 |

### デザイン特性による違い

順序回路（FF が多く単純なロジック — 例: 1000 個の並列カウンタ）と組み合わせ回路（深いロジックコーン — 例: SEC エンコーダ/デコーダ）では、同じチューニングに対して**逆の傾向**を示します:

- 順序回路はコミット操作が多い → `commitSinking`、`splitWideCommits`、`eliminateDeadWorkingStores`、`reschedule` が重要。
- 組み合わせ回路は深いロジックコーンを持つ → `optimizeBlocks` が重要。コミット関連パスの影響は小さい。
- 一方のデザインでコンパイルを遅くするパスが、他方では速くすることがある。

**両方のデザインタイプを均一に改善するデフォルト以外の設定は存在しません。** 必ず実際のワークロードでベンチマークしてください。

## Cranelift バックエンドオプション

SIRT パスとは別に、Cranelift 自体のコード生成を制御します。

| オプション | デフォルト | 説明 |
|---|---|---|
| `craneliftOptLevel` | `"speed"` | `"none"` / `"speed"` / `"speedAndSize"` |
| `regallocAlgorithm` | `"backtracking"` | `"backtracking"`（高品質コード）/ `"singlePass"`（高速コンパイル）|
| `enableAliasAnalysis` | `true` | egraph パスでのエイリアス解析 |
| `enableVerifier` | `true` | IR の正当性検証 |

### デザインタイプ別の影響

| オプション | 順序回路 (compile / tick) | 組み合わせ回路 (compile / eval) |
|---|---|---|
| `craneliftOptLevel: "none"` | −5% / −13% | **+27% / +123%** |
| `regallocAlgorithm: "singlePass"` | −16% / **+291%** | +33% / +31% |
| `enableAliasAnalysis: false` | −7% / −26% | +6% / +8% |
| `enableVerifier: false` | **−31%** / −26% | +6% / +12% |

ポイント:

- **`craneliftOptLevel: "none"`** は順序回路には有効だが、**組み合わせ回路には壊滅的**（eval +123%）。
- **`regallocAlgorithm: "singlePass"`** はコンパイル時間を短縮するが、順序回路のシミュレーションが **3〜4 倍遅くなる**。
- **`enableVerifier: false`** は順序回路で最大のコンパイル時間短縮（−31%）を得られるが、組み合わせ回路での効果はわずか。
- **`enableAliasAnalysis: false`** はどちらの方向にも小さな効果。

## ベンチマーク

自分のデザインでの影響を測定するベンチマークツールが付属しています:

```bash
cargo run --release --example pass_benchmark -p celox
```

2 つの代表的なデザイン（1000 カウンタの順序回路と SEC エンコーダ/デコーダの組み合わせ回路）に対し、個別パスの無効化、組み合わせ効果、Cranelift オプションをテストします。

フェーズごとの時間計測には環境変数を使えます:

```bash
# フェーズ別タイミング（parse, optimize, JIT）
CELOX_PHASE_TIMING=1 cargo test my_test_name

# バッチごとの JIT コンパイル詳細
CELOX_PASS_TIMING=1 cargo test my_test_name
```

## 推奨設定

| 目的 | 設定 |
|---|---|
| 最速のシミュレーション | デフォルト（全有効）|
| 最速のコンパイル | `craneliftOptLevel: "none"`, `enableVerifier: false` — ただし要ベンチマーク |
| 高速イテレーション（コンパイル時間優先）| `optimize: false` + Cranelift デフォルト、または Rust の `fast_compile()` |
| 本番シミュレーション | デフォルト — コンパイルコストは多数のシミュレーションサイクルで回収される |
