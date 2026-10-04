# lydite

Rustで実装したハードウェア状態対応チェッカー。仕様と実装のreset・遷移・bindingを検査し、commit時の仕様ステップと、それ以外のstutterを対応づけます。入力は `.lyd` または宣言的JSONです。

## はじめに

リポジトリの Rust ツールチェーン（`rust-toolchain.toml`）と Python 3.12 を使用します。既定のsolver経路にはZ3が必要です。有限Bool/BV経路は `LYDITE_SOLVER=finite` で選択し、外部solverへfallbackしません。

```sh
cargo build --release --locked
# 構文・名前・型だけを検査（証明ではありません）
../target/release/lydite audit/lemma_candidates/counter.lyd --check --out /tmp/lydite-check
# 小さな例を検証。出力先には新しいディレクトリを指定してください
LYDITE_SOLVER=finite ../target/release/lydite audit/lemma_candidates/counter.lyd --out /tmp/lydite-counter
```

## ドキュメント

| 目的 | 参照先 |
|---|---|
| VS Codeで編集・補題の明示的な検査 | [Editor / LSP](docs/usage.md#editor-and-language-server) |
| 実行方法、終了コード、開発・CIチェック | [使い方](docs/usage.md) |
| `.lyd` の記法とJSONへの対応 | [言語](docs/language.md)、[JSON v2](docs/schema.md) |
| 関係仕様と再利用可能なモジュール | [v3仕様](docs/specifications.md)、[v4 scoped仕様](docs/scoped-specifications.md)、[expectationとtrace](docs/expectations.md) |
| 受理した要求の条件付き応答期限 | [v3/v4 bounded response](docs/specifications.md#conditional-bounded-response) |
| プログラムの不変条件・停止性 | [program契約](docs/program-contracts.md) |
| 補題を `.lyd` で書き、検査して使う | [補題](docs/lemmas.md) |
| 自動証明探索、分割、予算 | [証明エンジン](docs/automatic-proofs.md) |
| 何を証明し、何を信頼しているか | [信頼境界](docs/trust.md) |
| CPU例と現在のRV32I対象 | [小さなCPUモデル](docs/models.md)、[RV32I](docs/rv32i.md)、[メモリ合成](docs/memory-composition.md) |
| 再現テストと保存証跡 | [検査・証跡の入口](audit/README.md) |
| FPGA計測の再現と制限 | [合成・配置配線](https://github.com/tignear/hwverify/blob/ae8f84cca8859097ac2b0a7a4f6e311355c2b48d/synthesis/README.md) |

## 現在の検証範囲

- 選択した4段RV32I実装は、手動補題を含む経路で **436/436の保存条件と7個のglobal義務**、4/16/64語メモリとの合成を検査します。元の単一queryはUnknownのままです。
- 補題を与えない自動経路は **431/436**。5個のUnknownを含むため、全体の証明成功とは扱いません。
- Celox の再利用可能なテストスイート696件を証明付きで実行し、**675件通過＋21件の記録済み例外**（Veryl の言語制約6件、証明バックエンド未対応のテストベンチ12件、Celox 自身も ignore している既知の失敗3件）を CI の coverage contract としています。全696件成功という意味ではありません。
- `.lyd` の補題も自動探索も同じ型付き候補APIを使います。候補や保存済みレポートは証明ではなく、現在のqueryに対する新しい検査が必要です。

実際のcheckoutの合否は[CI](../.github/workflows/lydite.yml)と新しい実行結果で確認してください。過去の集計やハッシュは現在の証明を代替しません。Rustの変換・義務生成・カーネル・選択したsolverを信頼しており、処理系全体の形式的な正しさは証明していません。
