# Vite プラグイン

`@celox-sim/vite-plugin` パッケージは、Veryl ソースファイルと TypeScript/Vitest ツールチェーンのシームレスな統合を提供します。

## 機能

プラグインは 3 つのことを自動的に処理します：

1. **モジュール解決** -- テストファイルで `import { Counter } from "../src/Counter.veryl"` が動作するようにします。
2. **型生成** -- TypeScript が各モジュールの形状（ポート、イベント、型）を理解できるように `.d.veryl.ts` サイドカーファイルを生成します。
3. **ホットリロード** -- `.veryl` ファイルが変更されると、プラグインはキャッシュを無効化して型を再生成します。

内部的には、プラグインは NAPI アドオンを介して `celox-ts-gen` 型ジェネレータを呼び出します。ジェネレータを手動で実行する必要はありません。

## インストール

```bash
pnpm add -D @celox-sim/vite-plugin
```

## 設定

### 基本設定

```ts
// vitest.config.ts
import { defineConfig } from "vitest/config";
import celox from "@celox-sim/vite-plugin";

export default defineConfig({
  plugins: [celox()],
});
```

プラグインは Vite プロジェクトルートから上方に探索して、最も近い `Veryl.toml` を自動的に見つけます。

### カスタムプロジェクトルート

`Veryl.toml` が Vite ルートまたはその親ディレクトリにない場合、パスを明示的に指定します：

```ts
export default defineConfig({
  plugins: [
    celox({
      projectRoot: "./path/to/veryl-project",
    }),
  ],
});
```

### tsconfig.json

`.veryl` インポートの TypeScript サポートを有効にするには、`tsconfig.json` に以下を追加します：

```json
{
  "compilerOptions": {
    "allowArbitraryExtensions": true,
    "rootDirs": ["src", ".celox/src"]
  },
  "include": ["test", "src", ".celox/src"]
}
```

- `allowArbitraryExtensions` は TypeScript が `.d.veryl.ts` ファイルを解決できるようにします。
- `rootDirs` は TypeScript に `.celox/` サイドカーディレクトリをソースツリーの仮想オーバーレイとして扱うように指示します。

## 生成されるファイル

プラグインはソースツリーをミラーリングして `.celox/` ディレクトリにサイドカーファイルを生成します：

```
my-project/
├── src/
│   └── Counter.veryl          # Veryl ソース
├── .celox/
│   └── src/
│       └── Counter.d.veryl.ts # 生成された型定義
└── vitest.config.ts
```

`.celox/` を `.gitignore` に追加してください：

```
.celox/
```

## クエリパラメータ

### `?dse=` — デッドストア除去

インポートパスに `?dse=` を付けると、インポートされるモジュールの[デッドストア除去](./dead-store-elimination.md)が有効になります：

```typescript
import { Top } from "../src/Top.veryl?dse=preserveAllPorts";
```

| 値 | 動作 |
|---|---|
| `?dse=preserveTopPorts` | トップモジュールのポートのみ DSE で保持 |
| `?dse=preserveAllPorts` | すべてのインスタンスのポートを DSE で保持 |
| `?dse`（値なし） | `preserveAllPorts` がデフォルト |

ポリシーは `ModuleDefinition` の `defaultOptions.deadStorePolicy` に埋め込まれ、`Simulator.create()` や `Simulation.create()` の呼び出し時に自動適用されます。呼び出し側のオプションがデフォルトをオーバーライドします。

## プラグインオプション

| オプション | 型 | デフォルト | 説明 |
|---|---|---|---|
| `projectRoot` | `string` | *（自動検出）* | `Veryl.toml` を含むディレクトリへのパス |
