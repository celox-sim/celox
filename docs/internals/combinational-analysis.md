# SLT 組合せ回路解析ガイド

## 概要

本ドキュメントでは、シミュレータが組合せ回路（`always_comb` ブロック）をどのように解析し、
実行可能な命令列に変換するかを説明する。

組合せ回路の処理は以下のパイプラインで行われる：

```
always_comb ブロック (veryl_analyzer::ir)
    │
    ▼  記号的評価 (comb.rs)
LogicPath<VarId>  ──  NodeId参照 + ソース依存情報
    │
    ▼  フラット化 (flatting.rs)
LogicPath<AbsoluteAddr>
    │
    ▼  atomize (flatting.rs)
LogicPath<AbsoluteAddr>  ──  ビット境界で分割済み
    │
    ▼  トポロジカルソート + lowering (scheduler.rs + lower.rs)
ExecutionUnit<AbsoluteAddr>  ──  SIR 命令列
```

## SLTNode（Symbolic Logic Tree）

`SLTNode<A>` は組合せ回路の式を表現する木構造である。
現行実装では、ノードは `SLTNodeArena` に保持され、式は `NodeId` で参照される。

```rust
pub enum SLTNode<A> {
    // 入力変数の参照
    Input {
        variable: A,                    // 変数アドレス
        index: Vec<NodeId>,             // 動的インデックス式（多次元対応）
        access: BitAccess,             // 参照するビット範囲
    },

    // 定数
    Constant(BigUint, usize),           // (値, ビット幅)

    // 二項演算
    Binary(NodeId, BinaryOp, NodeId),

    // 単項演算
    Unary(UnaryOp, NodeId),

    // 条件選択（if文から生成）
    Mux {
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
    },

    // ビット結合（{a, b} や部分代入の再構成）
    Concat(Vec<(NodeId, usize)>),       // (式参照, ビット幅) のリスト

    // ビットスライス（v[7:0] など）
    Slice {
        expr: NodeId,
        access: BitAccess,
    },
}
```

### `Input` ノードの動的インデックス

配列の動的アクセス `arr[i][j]` は以下のように表現される：

```
Input {
    variable: arr の VarId,
    index: [NodeId(i の式), NodeId(j の式)],
    access: BitAccess { lsb: 0, msb: element_width - 1 },
}
```

`index` が空の場合は静的アクセスであり、`access` のみでビット位置が確定する。

## LogicPath — データパスの表現

`LogicPath` は組合せ回路における1つのデータパスを表現する。
「どの変数のどのビット範囲が、どの式で、どの入力に依存して決定されるか」を記述する。

```rust
pub struct LogicPath<A> {
    pub target: VarAtomBase<A>,              // 書き込み先（変数 + ビット範囲）
    pub sources: HashSet<VarAtomBase<A>>,     // 読み出し元の集合
    pub expr: NodeId,                         // 値を計算する式木の参照
}
```

### `VarAtomBase` — ビット範囲付き変数参照

```rust
pub struct VarAtomBase<A> {
    pub id: A,              // 変数アドレス
    pub access: BitAccess,  // ビット範囲 [lsb, msb]
}
```

### 例

```systemverilog
always_comb {
    y = a + b;
}
```

この場合、以下の `LogicPath` が生成される：

```
LogicPath {
    target: VarAtom { id: y, access: [0, width-1] },
    expr: n42,  // 例: Arena上のノードID
    sources: { VarAtom(a, [0, width-1]), VarAtom(b, [0, width-1]) },
}
```

## 記号的評価アルゴリズム

### エントリポイント: `parse_comb`

`parse_comb` は `CombDeclaration`（`always_comb` ブロック）を受け取り、
`CombResult`（`LogicPath` のリストとビット境界マップ）を返す。

```
parse_comb(module, decl) → CombResult { paths, boundaries }
```

### SymbolicStore — 記号的状態

`SymbolicStore` は各変数の現在の記号的な値を管理するデータ構造である。

```rust
pub type SymbolicStore<A> =
    HashMap<VarId, RangeStore<Option<(NodeId, HashSet<VarAtomBase<A>>)>>>;
```

構造を分解すると：

- 外側の `HashMap<VarId, ...>`: 変数ごとのエントリ
- `RangeStore<...>`: ビット範囲ごとの式を管理（後述）
- `Option<...>`: `None` = 未変更、`Some` = 代入済み
- `(NodeId, HashSet<VarAtomBase>)`: (式木参照, ソース依存集合) のペア

初期状態では全変数が `None`（未変更）で初期化される。
代入文が評価されるたびに、対象変数の対応するビット範囲が `Some(...)` に更新される。

### RangeStore — ビット範囲の管理

`RangeStore<T>` はビット範囲ごとに値を管理する区間マップである。

```rust
pub struct RangeStore<T> {
    pub ranges: BTreeMap<usize, (T, usize)>,  // key: lsb, value: (値, 幅)
}
```

主要操作：

| メソッド | 説明 |
|---|---|
| `new(initial, width)` | 全ビット範囲を `initial` で初期化 |
| `split_at(bit)` | 指定ビット位置で範囲を分割 |
| `update(access, value)` | 指定ビット範囲の値を更新 |
| `get_parts(access)` | 指定範囲内の全パーツを取得 |

これにより部分代入が正確に追跡される。

#### 例: 部分代入の追跡

```systemverilog
logic [7:0] y;
always_comb {
    y[3:0] = a;
    y[7:4] = b;
}
```

```
初期状態:  RangeStore: { 0: (None, 8) }

y[3:0] = a の後:
  split_at(0), split_at(4)
  update([0,3], Some(Input(a)))
  RangeStore: { 0: (Some(Input(a)), 4), 4: (None, 4) }

y[7:4] = b の後:
  update([4,7], Some(Input(b)))
  RangeStore: { 0: (Some(Input(a)), 4), 4: (Some(Input(b)), 4) }
```

### 文の評価

#### `eval_assign` — 代入文

静的インデックスの代入を処理する。RHS の式を記号的に評価し、結果を `SymbolicStore` に書き込む。

```
eval_assign(module, store, boundaries, stmt)
  → (updated_store, updated_boundaries)
```

1. RHS の式を `eval_expression` で評価 → `(NodeId, sources)`
2. LHS のビット範囲を計算
3. `store[lhs_var].update(access, Some((expr, sources)))` で記号的状態を更新

#### `eval_dynamic_assign` — 動的インデックス代入

`arr[i] = value` のような動的インデックスへの代入を処理する。
動的インデックスの場合、書き込み先のビット位置が実行時にしか決まらないため、
変数全体のビット範囲を対象とする `LogicPath` を即座に生成する。

#### `eval_if` — 条件文

`if` 文の各分岐を独立に評価し、結果を `Mux` ノードで合成する。

```
eval_if(module, store, boundaries, stmt)
```

1. 条件式を評価 → `cond_node`
2. then ブランチを `store` のクローンで評価 → `then_store`
3. else ブランチを `store` のクローンで評価 → `else_store`
4. 各変数について `then_store` と `else_store` の結果を `Mux` で合成

**重要**: `else` 節がない場合、未代入のビット範囲は `None`（未変更）のまま残る。
最終的に `None` のパーツは `Input`（自身の現在値）として復元される。
これは組合せ回路のラッチ推論に対応する。

### ビット境界（Boundary）の収集

`BoundaryMap<A>` は各変数について、ビット境界の集合を保持する。

```rust
pub type BoundaryMap<A> = HashMap<A, BTreeSet<usize>>;
```

境界は式の評価中に自動的に収集される。変数のビットスライス `v[7:4]` が参照されると、
`v` の境界セットにビット位置 `4` と `8` が追加される。

### LogicPath の最終生成

`parse_comb` の最終段階で、`SymbolicStore` から `LogicPath` を生成する：

1. 各変数の `RangeStore` から `Some(...)` のパーツ（＝代入された範囲）を取得
2. 恒等変換（`Input(self)` への代入）は除外
3. 残りの各パーツに対して `LogicPath` を生成

### `combine_parts` — パーツの結合

`combine_parts` は複数のビット範囲パーツを1つの式に結合する。

```rust
combine_parts(parts: Vec<((NodeId, sources), BitAccess)>) -> (NodeId, sources)
```

- パーツが1つの場合: そのまま返す
- パーツが複数の場合: `Concat` ノードで結合する

`combine_parts_with_default` は `None`（未変更）パーツを含む場合に使用し、
`None` の箇所には `Input`（現在値の参照）を挿入する。

## Atomize — ビット境界による分割

フラット化後、複数モジュールの `LogicPath` を統合する際に、
異なるモジュールが同一変数の異なるビット範囲を参照する場合がある。

`atomize_logic_paths` は境界マップに基づき、各 `LogicPath` を最小のビット単位（atom）に分割する。
これにより、スケジューラが正確な依存関係を構築できる。

```
atomize_logic_paths(paths, boundaries) → atomized_paths
```

各 `LogicPath` のターゲットとソースの `BitAccess` が境界で分割され、
必要に応じて `Slice` ノードが挿入される。

## スケジューリング

`scheduler::sort` が全ての `LogicPath` をトポロジカルソートし、`ExecutionUnit` を生成する。

### アルゴリズム

1. **空間インデックスの構築**: 各変数のどのビット範囲をどの `LogicPath` が駆動するかをマッピング
2. **多重ドライバ検出**: 同一ビット範囲を複数のパスが駆動していればエラー
3. **依存グラフの構築**: 各パスのソースがどのパスのターゲットと重なるかを検査し、辺を追加
4. **Kahn のアルゴリズム**: トポロジカルソートを実行。サイクルがあれば `CombinationalLoop` エラー
5. **SIR 生成**: ソート順に各 `LogicPath` の `expr(NodeId)` を `SLTToSIRLowerer` で SIR に変換

### エラー

```rust
pub enum SchedulerError<A> {
    CombinationalLoop { blocks: Vec<LogicPath<A>> },
    MultipleDriver { blocks: Vec<LogicPath<A>> },
}
```

## SLT → SIR Lowering

`SLTToSIRLowerer` が `SLTNode` を再帰的に SIR 命令列に変換する。

主要な変換ルール：

| SLTNode | SIR |
|---|---|
| `Input` | `Load` 命令（動的インデックスがある場合はオフセット計算を含む） |
| `Constant` | `Imm` 命令 |
| `Binary` | 左右を再帰 lowering → `Binary` 命令 |
| `Unary` | オペランドを再帰 lowering → `Unary` 命令 |
| `Mux` | `Branch` 終端命令による条件分岐 |
| `Concat` | 各パーツを lowering → シフト + OR で結合 |
| `Slice` | 式を lowering → シフト + マスク |

### Mux の Lowering

`Mux` は制御フローに変換される：

```
Block_current:
    cond_reg = lower(cond)
    Branch { cond: cond_reg, true: (Block_then, []), false: (Block_else, []) }

Block_then:
    then_reg = lower(then_expr)
    Jump(Block_merge, [then_reg])

Block_else:
    else_reg = lower(else_expr)
    Jump(Block_merge, [else_reg])

Block_merge (params: [result_reg]):
    ... 後続の処理 ...
```

これにより短絡評価が自然に実現される（選択されなかった分岐の式は評価されない）。

## 関連ドキュメント

- [アーキテクチャ概要](./architecture.md) — シミュレータ全体の設計
- [SIR 中間表現リファレンス](./ir-reference.md) — lowering 先の SIR 命令セット詳細
- [最適化アルゴリズム](./optimizations.md) — ハッシュ・コンシングやホイスティングの詳細
