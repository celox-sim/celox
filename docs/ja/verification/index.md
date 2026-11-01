# 検証結果

<script setup>
import VerificationResults from '../../.vitepress/components/VerificationResults.vue'
</script>

固定リビジョンの [sv-tests](https://github.com/chipsalliance/sv-tests) に対する
Celox の結果です。**Main tests・Cores・Imported** を切り替え、タグや機能名で検索できます。
[上流ダッシュボード](https://chipsalliance.github.io/sv-tests-results/)を参考にした構成で、
Celox の結果だけを掲載しています。

<VerificationResults />

## 結果の読み方

**Main tests** は master のレビュー済み `expected.tsv` から集計します。
複数回の実行で確認した判定を保持する期待値であり、新しい全件測定ではありません。
出典には期待値を保持しているソースコミットを表示します。
単一の実測日時やシミュレータービルドを表すものではありません。
master の期待値を更新すると、次のサイト公開で反映されます。

**Cores・Imported** は PR の合否に追加せず、独立した daily・手動実行で測定します。
ページの初期表示には保存済みの実測値を使い、新しい完全な daily 結果があれば読み込みます。
各グループの測定日時・リビジョンは独立して表示し、収集失敗時は直前の完全な結果を保持します。
ケース別結果・診断・ログは daily 実行の artifact に14日間保存します。

スキップは実行数の分母から除外します。実行数が0の行は「—」と表示し、成功率0%とは区別します。
一部の core ケースは upstream の `compatible-runners` に Celox が含まれないためスキップされます。
CVA6 のテストベンチ1件に残る存在しない include パス2つは、言語機能ではなく upstream の入力不備として表示します。

タグは重複しますが、全体件数では各ケースを1回だけ数えます。2state と4state は別々に集計します。
構文解析やエラボレーションの成功は、シミュレーションの正しさを保証しません。
異常系テストは未対応機能のエラーでも成功する場合があります。
また、imported には表示されたチェックの内容を検証せず終了コードだけを判定するケースがあり、
公式判定が成功でも出力に `Failed` が残る場合があります。
これは公式テストの成功率であり、言語仕様の対応率ではありません。
実行モードと診断の内訳は表の下から確認できます。

ローカル測定は `heliodor-dev`、daily は `release` を使います。
公式のタイムアウトを適用するため、ビルド設定やマシン負荷によって結果が変わる場合があります。
