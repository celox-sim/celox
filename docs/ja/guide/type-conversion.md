# 型変換

このページでは、`celox-ts-gen` がモジュール定義を生成する際に Veryl の型が TypeScript の型にどのようにマッピングされるかを説明します。

## 変換テーブル

| Veryl の型 | TS の型 | 4 値 | 備考 |
|---|---|---|---|
| `clock` | *（ポートから除外）* | yes | `addClock()` / `tick()` でイベントとして扱われる |
| `reset` | `bigint` | yes | |
| `logic<N>` | `bigint` | yes | |
| `bit<N>` | `bigint` | no | 2 値のみ |
| `f32` / `f64` | `bigint` | no | IEEE 754 のビットパターン。スカラーの算術演算・比較・f32/f64 間の変換・64 ビット以下の整数との変換に対応。`%f`・`%e`・`%g` による数値表示と幅・精度指定に対応。配列などの集約型の数値変換と64ビットを超える整数との変換は未対応([#1653](https://github.com/celox-sim/celox/issues/1653)) |
| `string` | — | no | 定数でない string はテストベンチのコードでのみ使える。RTL では文字列リテラル・定数・パラメータを使える([#1400](https://github.com/celox-sim/celox/issues/1400)) |

すべてのシグナルポート値はビット幅に関わらず `bigint` を使用します。これにより、すべてのシグナルで一貫した型が保証され、シグナル幅の変更時に型が変わることがなくなります。

浮動小数点から整数へのキャストは最近接整数へ丸め、ちょうど中間の場合はゼロから遠ざかる方向へ丸めます。
SystemVerilog の `$rtoi` は切り捨てです。Celox は NaN と無限大を整数のゼロに変換し、
有限値が変換先の整数範囲を超えた場合は下位ビットを保持します。
この範囲外の動作にはシミュレータ間の移植性を期待しないでください。

Veryl analyzer 0.22 には、負の `f32` へのキャストや定数・パラメータ間の依存を含む、
一部の浮動小数点定数式を誤評価する問題があります。
上流の制限として [#1657](https://github.com/celox-sim/celox/issues/1657) で追跡しています。

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
    at(i: number): bigint;
    readonly length: number;
  };
}
```

入力配列ポートの場合、`set(i, value)` メソッドも生成されます：

```ts
interface MyPorts {
  data: {
    at(i: number): bigint;
    set(i: number, value: bigint): void;
    readonly length: number;
  };
}
```

配列インデックスには `0 <= i < length` を満たす整数を指定する必要があります。
無効なインデックスを指定すると、`at()` と `set()` は `RangeError` を送出します。

## 4 値と 2 値

| 型 | 4 値 |
|---|---|
| `logic` | yes |
| `clock` | yes |
| `reset` | yes |
| `bit` | no |

`SimulatorOptions` で `fourState: true` を有効にすると、4 値シグナルは値と一緒に追加のマスクを持ちます。マスクビットが 1 のビットは不定値（X）を示します。4 値の値を構築するには `FourState()` ヘルパーを使用し、アサーションには vitest マッチャー（`toBeX`、`toBeAllX`、`toBeNotX`）を使用してください。

詳しくは [4 値シミュレーション](./four-state.md) を参照してください。
