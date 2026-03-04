# デッドストア除去 (DSE)

デッドストア除去は、読み取られることのない信号へのストアを削除し、より高速な JIT コードを生成します。テスト時にほとんどの内部信号が観測されない大規模設計で特に効果的です。

## ポリシー

| ポリシー | 動作 |
|---|---|
| `"off"`（デフォルト） | 除去なし — すべてのストアを保持 |
| `"preserveTopPorts"` | トップモジュールのポートへのストア以外を除去 |
| `"preserveAllPorts"` | **全インスタンス**のポートへのストア以外を除去 |

## 使い方

### `fromSource` / `fromProject` の場合

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  deadStorePolicy: "preserveTopPorts",
});
```

### Vite プラグインの場合

インポートパスに `?dse=` クエリパラメータを付けます：

```typescript
import { Top } from "../src/Top.veryl?dse=preserveAllPorts";

const sim = Simulator.create(Top);
```

ポリシーは `ModuleDefinition` の `defaultOptions.deadStorePolicy` に組み込まれます。呼び出し時にオーバーライドも可能です：

```typescript
// モジュールは ?dse=preserveAllPorts だが、この呼び出しでは preserveTopPorts を使う
const sim = Simulator.create(Top, {
  deadStorePolicy: "preserveTopPorts",
});
```

`?dse`（値なし）の場合は `"preserveAllPorts"` がデフォルトになります（安全側）。

## ポリシーの選び方

### `preserveTopPorts`

トップモジュールの I/O のみが必要な**パフォーマンス重視のベンチマーク**に最適です。サブインスタンスの信号は除去されるため、`sim.dut.u_sub` は `undefined` になります：

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  deadStorePolicy: "preserveTopPorts",
});

sim.dut.top_in = 0xABn;
sim.tick();
expect(sim.dut.top_out).toBe(0xABn);

// 子インスタンスは利用不可
expect((sim.dut as any).u_sub).toBeUndefined();
```

### `preserveAllPorts`

**一般的なテスト**に最適です。DSE のパフォーマンス向上を得つつ、すべてのインスタンスのポートにデバッグ用にアクセスできます：

```typescript
const sim = Simulator.fromSource(SOURCE, "Top", {
  deadStorePolicy: "preserveAllPorts",
});

sim.dut.top_in = 0x42n;
sim.tick();
expect(sim.dut.top_out).toBe(0x42n);

// 子インスタンスのポートはアクセス可能
expect(sim.dut.u_sub.o_data).toBe(0x42n);
```

::: tip 内部変数について
どちらの DSE ポリシーでも、実行ユニットが読み取らない内部変数（ポートでない `var` 宣言）は除去される可能性があります。ポート信号のみが保持されることが保証されます。
:::

## 仕組み

1. Rust オプティマイザが、どのストアが実行ユニットによってロードされるかを解析します。
2. ロードされないストアが除去候補になります。
3. ポリシーにより**外部ライブ**信号が保護対象に追加されます：
   - `PreserveTopPorts` — トップモジュールのポートアドレスのみ。
   - `PreserveAllPorts` — 設計内のすべてのインスタンスのポートアドレス。
4. TypeScript 側では `filterHierarchyForDse()` が DUT 階層を調整し、除去されたサブインスタンスが壊れたアクセサとして公開されないようにします。

## 関連資料

- [子インスタンスへのアクセス](./hierarchy.md) — DUT 階層の仕組み。
- [Vite プラグイン](./vite-plugin.md) — プラグインの設定とクエリパラメータ。
- [テストの書き方](./writing-tests.md) — Simulator・Simulation のパターン。
