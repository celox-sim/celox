# Heliodor Linux ベンチマーク

Heliodor は Celox の大規模な外部 Veryl ワークロードです。固定した Linux イメージを
起動し、同じ設計リビジョンと入力を使って Celox のネイティブバックエンド、tiered JIT、
Veryl-CC の同期版と tiered 版を CPU アーキテクチャごとに比較します。
Celox の tiered 版はインタプリタで実行を始め、バックグラウンドでネイティブコードを生成します。
x86-64 では先に軽量なネイティブコードへ切り替え、最適化版が完成すると安全な実行境界で
再度切り替えます。実行中の状態とイベントバッファは保持します。
コード生成のトレースを指定した場合は、最適化版を直接生成します。
Veryl-CC の tiered 版は Cranelift で実行を始め、C の非同期コンパイルが完了すると切り替えます。
これは通常の `veryl test --backend cc` と同じ `aot_c_async=true` の設定です。
同期版は明示的に `aot_c_async=false` とし、C のコンパイル完了を待ってから実行します。
Cranelift 単独の Linux 起動時間はこの比較の有効な尺度を大幅に
超えるため、計測・公開しません。

## 測定するもの

次の 3 つを分けて計測します。

1. 同期バックエンドが設計をコンパイルする時間。
2. テストベンチを含め、ワークロード全体を実行する時間。
3. コンパイルとシミュレーションを並行させる tiered 実行の起動から Linux 完了までの時間。

実行時間は両シミュレータともテストベンチの処理を含みます。tiered 版は「実行開始まで」「コンパイルと
並行した実行」「Linux 起動完了までの総時間」を別のグラフで表示します。tiered の実行区間には
切り替え前のバックエンドも含まれるため、生成コードだけの速度としては比較しません。
Veryl-CC は C のコンパイル完了前に Cranelift だけで処理を終える場合もあり、
その場合も有効な tiered の測定値として扱います。
起動時間と総時間は設計の解析前から測り、ソースファイルの読み込みと Cargo によるランナーの
ビルド時間は含みません。TSV の `compile_elapsed_ns` は tiered 版では実行開始までの時間を表し、
バックグラウンドのコンパイル全体の時間ではありません。Veryl-CC の同期版・tiered 版はそれぞれ
独立した空の AOT-C キャッシュを使います。過去の同期版の測定値は同じ系列に保持します。

途中までの起動、完了時間の推定、コンパイルだけの結果は、実行成功として扱いません。

## 有効な結果

次の条件をすべて満たす実行だけを採用します。

- 固定した Heliodor とワークロードのリビジョンを使う。
- 設定された Linux 完了マーカーまで到達する。
- コンパイル時間と実行時間を分けて記録する。
- 意図した Celox / Veryl リビジョンからランナーをビルドする。
- タイムアウトや意味上の不一致を調査できるログを残す。
- Celox の tiered 実行が Linux 完了前に生成コードへ昇格し、生成コードで 1 回以上評価したことを確認する。
- Veryl-CC の tiered 実行で C の非同期コンパイルが有効であり、生成コードかフォールバックで 1 回以上実行したことを確認する。

完了マーカーを固定することで、高速に失敗した実行や未完了の起動を性能向上として
誤って報告することを防ぎます。

## ローカル実行

```bash
bash scripts/run-heliodor-bench.sh run
```

両方の tiered バックエンドを比較する場合:

```bash
HELIODOR_RUNNERS="celox-tiered veryl-cc-tiered" bash scripts/run-heliodor-bench.sh run
```

CI の固定 `gate` は x86-64 で `veryl-cc-sync`、`celox`、`celox-tiered`、
`veryl-cc-tiered` を実行します。夜間の AArch64 ジョブも同じ 4 種類を測定し、
各アーキテクチャで両方の tiered 結果が揃った場合に公開します。

初回は固定した Heliodor checkout の取得にネットワークアクセスが必要です。スクリプトは
使用リビジョン、ビルド構成、完了状態、計測時間を表示します。変更前後の比較には同じ
マシンと構成を使ってください。

公開結果は[ベンチマークダッシュボード](./index.md)の **Heliodor Linux** に掲載します。

## 大規模・Linux バージョン別の測定

nightly とプロファイルを取らない手動実行では、次の 9 ケースを x86-64
（`ubuntu-24.04`）と AArch64（`ubuntu-24.04-arm`）で測定します。
各ケースで前述の 4 バックエンドを実行します。

| ゲスト Linux カーネル | hart 数 |
| --- | --- |
| 5.15 | 1、2、4、8 |
| 6.6 | 1、2、4 |
| 7.1 | 1 |
| 7.1（ベクトル有効） | 1 |

Heliodor のリビジョンは `6285682fa0a514077da9d17fee385c7841160025` に固定します。
Linux バージョンはシミュレーション内で起動するゲストのもので、ホスト OS の違いではありません。
同じ workload・アーキテクチャを一つのジョブにまとめ、全体で 18 ジョブを実行します。
各ジョブ内で 4 バックエンドを順番に実行するため、同じ問題の結果は同じ CPU で比較できます。
別の workload は別ホストでも構いません。CPU とホストの識別情報を成果物に記録し、
公開時には各ジョブの 4 バックエンドすべての成功を検証します。
各実行のタイムアウトは 1・2 hart が 1 時間、4 hart が 3 時間、8 hart が 5.5 時間です。未完了・失敗は計測値として公開せず、
両アーキテクチャの全ケースが成功した場合に nightly の結果を公開します。
8 hart の Veryl テストには `8hart-100m-v1` の調整を適用し、古い 3,000 万サイクルの
上限を、上流の Verilator ラッパーと同じ 1 億サイクルに揃えます。shutdown のアサーションは維持します。
`HELIODOR_SUITE=1` は固定した suite リビジョンだけにこの調整を適用し、ジョブログにも記録します。

Linux 7.1 SMP（2・4 hart）は RTL の修正待ちとして対象から除外しています。
どちらも一つの hart で命令の完了が停止し、2 hart では Verilator でも
データキャッシュの読み出し待ちと他 hart のロック待ちが再現しました。
この失敗を成功した計測結果として扱うことはありません。

別の HEAD 互換性ジョブでは、上流リビジョン
`94e9c5821c24a8941c3ddc3b76daddc7124a855a` だけを除外します。
この版のテストベンチには ROM／DRAM 初期化の `initial_assign` 属性がなく、
固定版 `6285682` で追加されています。CI には既知のソース不備による除外と明記し、
シミュレーション成功には数えません。それ以外の HEAD は検証対象のままです。

ダッシュボードにはカーネルと hart 数を区別して表示します。設計リビジョンが異なるため、
従来の固定 gate とは別の履歴です。コンパイル・実行・tiered の時間の定義は共通です。
大規模ケースは PR ごとには実行しません。

Linux 6.6 の 4 hart をローカルで実行する例:

```bash
HELIODOR_REF=6285682fa0a514077da9d17fee385c7841160025 \
HELIODOR_TESTS=test_soc_66_smp_linux_boot_4hart \
HELIODOR_RUNNERS="veryl-cc-sync celox celox-tiered veryl-cc-tiered" \
HELIODOR_CELOX_CARGO_PROFILE=release HELIODOR_TIMEOUT_SEC=10800 \
bash scripts/run-heliodor-bench.sh run
```

一部の構成だけを再実行する場合は、手動実行の `suite_test`、`suite_runner`、
`suite_arch` を指定します。空欄なら全構成が対象です。絞り込み実行では従来の gate を
実行せず、ダッシュボードの履歴も更新しません。例:

```bash
gh workflow run heliodor-bench.yml --ref <branch> \
  -f suite_test=test_soc_66_smp_linux_boot_4hart \
  -f suite_runner=celox -f suite_arch=aarch64
```

nightly・手動実行とも、既定では各 workload・アーキテクチャにつき **1 ジョブ**を作り、
4 バックエンドを同じ VM で順番に実行します。一部だけを比較する場合は、
`suite_runner` にスペース区切りで指定すると、その順序で実行します。
CPU 情報、ランナー名、run/attempt、boot ID を結果と一緒に保存します。
Veryl-CC の AOT-C キャッシュは各測定で新しく作成します。

```bash
gh workflow run heliodor-bench.yml --ref <branch> \
  -f suite_test=test_soc_smp_linux_boot_8hart \
  -f suite_runner="celox-tiered veryl-cc-tiered" -f suite_arch=x86_64
```

比較実行全体の上限はビルドを含めて 5.5 時間です。
[GitHub ホストジョブの 6 時間上限](https://docs.github.com/en/actions/reference/limits)までにログを保存するためです。
必要なバックエンドだけを選んでください。大規模な ARM の起動を複数並べると上限内に
収まらない場合があります。時間切れやバックエンドの結果欠落は比較失敗とし、途中結果を
完走ベンチマークとして公開しません。各バックエンドの個別上限もこの共通予算内で適用します。
別コミットを含む別 workflow run の履歴は、同一 CPU での比較ではありません。
