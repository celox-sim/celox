# hwverify

Rustで実装したハードウェア状態対応チェッカー。仕様と実装のreset・遷移・bindingを検査し、commit時の仕様ステップと、それ以外のstutterを対応づけます。入力は `.hwv` または宣言的JSONです。

## はじめに

Rust 1.98.1、Python 3.12を使用します。既定のsolver経路にはZ3が必要です。有限Bool/BV経路は `HWVERIFY_SOLVER=finite` で選択し、外部solverへfallbackしません。

```sh
cargo build --release --locked
# 構文・名前・型だけを検査（証明ではありません）
target/release/hwverify-rs audit/lemma_candidates/counter.hwv --check --out /tmp/hwverify-check
# 小さな例を検証。出力先には新しいディレクトリを指定してください
HWVERIFY_SOLVER=finite target/release/hwverify-rs audit/lemma_candidates/counter.hwv --out /tmp/hwverify-counter
```

## ドキュメント

| 目的 | 参照先 |
|---|---|
| VS Codeで編集・補題の明示的な検査 | [Editor / LSP](docs/usage.md#editor-and-language-server) |
| 実行方法、終了コード、開発・CIチェック | [使い方](docs/usage.md) |
| `.hwv` の記法とJSONへの対応 | [言語](docs/language.md)、[JSON v2](docs/schema.md) |
| 関係仕様と再利用可能なモジュール | [v3仕様](docs/specifications.md)、[v4 scoped仕様](docs/scoped-specifications.md)、[expectationとtrace](docs/expectations.md) |
| プログラムの不変条件・停止性 | [program契約](docs/program-contracts.md) |
| 補題を `.hwv` で書き、検査して使う | [補題](docs/lemmas.md) |
| 自動証明探索、分割、予算 | [証明エンジン](docs/automatic-proofs.md) |
| 何を証明し、何を信頼しているか | [信頼境界](docs/trust.md) |
| CPU例と現在のRV32I対象 | [小さなCPUモデル](docs/models.md)、[RV32I](docs/rv32i.md)、[メモリ合成](docs/memory-composition.md) |
| 再現テストと保存証跡 | [検査・証跡の入口](audit/README.md) |
| FPGA計測の再現と制限 | [合成・配置配線](synthesis/README.md) |

## 現在の検証範囲

- 選択した4段RV32I実装は、手動補題を含む経路で **436/436の保存条件と7個のglobal義務**、4/16/64語メモリとの合成を検査します。元の単一queryはUnknownのままです。
- 補題を与えない自動経路は **431/436**。5個のUnknownを含むため、全体の証明成功とは扱いません。
- 元のVerylコーパスは665件を実行し、**663件通過＋2件の明示した言語制約**をCIのcoverage contractとしています。全665件成功という意味ではありません。
- `.hwv` の補題も自動探索も同じ型付き候補APIを使います。候補や保存済みレポートは証明ではなく、現在のqueryに対する新しい検査が必要です。

実際のcheckoutの合否は[CI](.github/workflows/veryl-fv.yml)と新しい実行結果で確認してください。過去の集計やハッシュは現在の証明を代替しません。Rustの変換・義務生成・カーネル・選択したsolverを信頼しており、処理系全体の形式的な正しさは証明していません。
