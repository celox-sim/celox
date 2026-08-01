# はじめに

Celox は [Veryl HDL](https://veryl-lang.org/) 向けのシミュレータです。`.veryl`
モジュールを TypeScript から型安全に import し、入力を操作してクロックを進め、
Vitest で出力を検証できます。

::: tip ブラウザで試す
[Celox Playground](https://celox-sim.github.io/celox/playground/) では、ローカルに
ツールチェーンを入れずに Veryl 設計を実行できます。
:::

## できること

- Veryl モジュールを、生成されたポート型付きで TypeScript から import する。
- 組み合わせ回路と順序回路を、手動クロックまたはスケジュールクロックでテストする。
- 必要に応じて子インスタンスのポートへアクセスする。
- 4 値モードを有効にして `X` と `Z` を扱う。
- GTKWave や Surfer で確認できる VCD 波形を出力する。
- 値パラメータを上書きし、テスト専用の Veryl ソースを読み込む。

Celox はシミュレータ作成時に設計をコンパイルします。x86-64 ではネイティブ
バックエンド、それ以外のネイティブ環境では Cranelift JIT、Playground では
WebAssembly を使います。通常は自動選択され、TypeScript テストベンチ API は
変わりません。

## シミュレーション方法を選ぶ

イベントをテスト側で明示的に制御する場合は `Simulator` を使います。サイクル単位の
ユニットテストに向いています。

```typescript
const sim = Simulator.create(Counter);
sim.dut.enable = 1n;
sim.tick();
expect(sim.dut.count).toBe(1n);
sim.dispose();
```

クロックのスケジュールやシミュレーション時刻が必要な場合は `Simulation` を使います。
マルチクロックや時間ベースのテストに向いています。

## 次に読むページ

[はじめる](./getting-started.md)でプロジェクトをセットアップし、
[テストの書き方](./writing-tests.md)でテストベンチ API を確認してください。
[API リファレンス](/api/)には TypeScript のクラスとオプションを掲載しています。

コンパイラとランタイムの設計は、ユーザーガイドとは分けて
[シミュレータアーキテクチャ](/internals/architecture)にまとめています。
