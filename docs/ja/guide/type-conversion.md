# 型変換

このページでは、`celox-ts-gen` がモジュール定義を生成する際に Veryl の型が TypeScript の型にどのようにマッピングされるかを説明します。

## 変換テーブル

| Veryl の型 | ビット幅 | TS の型 | 4 値 | 備考 |
|---|---|---|---|---|
| `clock` | 1 | *（ポートから除外）* | yes | `addClock()` / `tick()` でイベントとして扱われる |
| `reset` | 1 | `number` | yes | |
| `logic<N>` (N &le; 53) | N | `number` | yes | |
| `logic<N>` (N &gt; 53) | N | `bigint` | yes | |
| `bit<N>` (N &le; 53) | N | `number` | no | 2 値のみ |
| `bit<N>` (N &gt; 53) | N | `bigint` | no | 2 値のみ |

## 53 ビット閾値

JavaScript の `number` は IEEE 754 倍精度浮動小数点数で、2<sup>53</sup> &minus; 1（`Number.MAX_SAFE_INTEGER`）までの整数を正確に表現できます。53 ビットを超えるシグナルには、暗黙的な精度損失を避けるために `bigint` が使用されます。

## 方向とミュータビリティ

| 方向 | 読み取り | 書き込み | TS 修飾子 |
|---|---|---|---|
| `input` | yes | yes | *（ミュータブル）* |
| `output` | yes | no | `readonly` |
| `inout` | yes | yes | *（ミュータブル）* |

出力ポートは生成される `Ports` インターフェースで `readonly` として宣言されます。出力ポートへの代入は TypeScript のコンパイルエラーになります。

## クロックポート

`clock` 型のポート（`clock_posedge` や `clock_negedge` を含む）は、生成される `Ports` インターフェースには**含まれません**。代わりに、モジュールの `events` 配列に表示され、以下で使用されます：

- イベントベースシミュレーションでの `Simulator.tick()` / `Simulator.event()`
- タイムベースシミュレーションでの `Simulation.addClock()`

## 配列ポート

配列ポート（例: `output logic<32>[4]`）はインデックスアクセスを持つオブジェクトとして表現されます：

```ts
interface CounterPorts {
  readonly cnt: {
    at(i: number): number;
    readonly length: number;
  };
}
```

入力配列ポートの場合、`set(i, value)` メソッドも生成されます：

```ts
interface MyPorts {
  data: {
    at(i: number): number;
    set(i: number, value: number): void;
    readonly length: number;
  };
}
```

## 4 値と 2 値

| 型 | 4 値 |
|---|---|
| `logic` | yes |
| `clock` | yes |
| `reset` | yes |
| `bit` | no |

`SimulatorOptions` で `fourState: true` を有効にすると、4 値シグナルは値と一緒に追加のマスクを持ちます。マスクビットが 1 のビットは不定値（X）を示します。4 値の値を構築するには `FourState()` ヘルパーを使用し、アサーションには vitest マッチャー（`toBeX`、`toBeAllX`、`toBeNotX`）を使用してください。

詳しくは [4 値シミュレーション](./four-state.md) を参照してください。
