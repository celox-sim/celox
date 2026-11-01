# 検証結果

<script setup>
import VerificationResults from '../../.vitepress/components/VerificationResults.vue'
</script>

## SystemVerilog: sv-tests

<VerificationResults />

sv-tests の imported グループを毎日・手動実行で測定します。PR の CI には追加しません。
main グループの既存検証は継続します。cores グループは今回の測定対象に含みません。

公式 runner が成功・失敗・スキップを判定します。成功率の分母からスキップを除外し、
2state と 4state を個別に表示します。構文解析・エラボレーションの成功は、
シミュレーションの正しさを保証しません。異常系テストは未対応機能によるエラーでも
成功する場合があります。これはテストの成功率であり、言語仕様の対応率ではありません。

初期表示には今回のローカル測定結果を使用し、新しい daily の完全な結果があれば
読み込みます。収集に失敗した場合は直前の結果を保持します。詳細なケース別結果・
診断・ログは daily 実行の artifact に14日間保存します。

初回測定は最適化した `heliodor-dev`、daily は `release` を使います。
公式のタイムアウトを適用するため、ビルド設定やマシン負荷によって結果が変わる場合があります。
