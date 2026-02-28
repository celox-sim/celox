# SIR 中間表現リファレンス

SIR (Simulator Intermediate Representation) は `veryl-simulator` の実行用 IR です。
Veryl の解析結果をレジスタベース命令列へ落とし込み、Cranelift JIT の入力になります。

## 概要

-   **レジスタベース**: 仮想レジスタ (`RegisterId`) による SSA 風表現
-   **CFG 表現**: `BasicBlock` + `SIRTerminator` による制御フロー
-   **領域付きメモリ**: `RegionedAbsoluteAddr` と `SIROffset` によるビット精度アクセス

## アドレス体系

| 型 | 用途 | 段階 |
| :--- | :--- | :--- |
| `VarId` | モジュール内ローカル変数 ID | `SimModule` 内部 |
| `AbsoluteAddr` | グローバル変数 (`InstanceId` + `VarId`) | フラット化後 |
| `RegionedAbsoluteAddr` | メモリ領域（Stable/Working）付きアドレス | 実行・最適化 |
| `SignalRef` | 実行用物理メモリアドレスハンドル | 実行（高速アクセス） |

## 主要データ構造

### `Program`

シミュレーション全体を表現する構造体です。FF 評価が 3 つの種類に分かれているのが特徴です。

```rust
pub struct Program {
    pub eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    // ... その他メタデータ
}
```

-   **`eval_apply_ffs`**: 通常の FF 同期評価。単一ドメイン動作時に使用。
-   **`eval_only_ffs`**: 次状態の計算のみを行い、Working 領域に書き込むフェーズ。
-   **`apply_ffs`**: Working 領域から Stable 領域へ値を確定させるフェーズ。

### `ExecutionUnit`

実行の最小単位です（※実装上の綴りは `ExecutionUnit` です）。

```rust
pub struct ExecutionUnit<A> {
    pub entry_block_id: BlockId,
    pub blocks: HashMap<BlockId, BasicBlock<A>>,
    pub register_map: HashMap<RegisterId, RegisterType>,
}
```

## 命令セット

-   `Imm(rd, value)`: 即値代入
-   `Binary(rd, rs1, op, rs2)`: 二項演算
-   `Unary(rd, op, rs)`: 単項演算
-   `Load(rd, addr, offset, bits)`: メモリ読み込み
-   `Store(addr, offset, bits, rs)`: メモリ書き込み (RMW)
-   `Commit(src, dst, offset, bits)`: 領域間コピー
-   `Concat(rd, [msb..lsb])`: レジスタ連結

## 制御フロー

-   `Jump(block_id, args)`: 無条件遷移（ブロック引数付き）
-   `Branch { cond, true_block, false_block }`: 条件分岐
-   `Return`: 実行終了
-   `Error(code)`: ランタイムエラー
