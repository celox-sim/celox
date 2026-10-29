# ベンチマーク

Celox は、各バックエンドおよび Verilator との間でコンパイル時間とシミュレーション
速度を継続的に計測しています。ダッシュボードは傾向の確認に使うもので、すべての
RTL 設計の性能を予測するものではありません。

## ダッシュボード

<ClientOnly><BenchmarkDashboard /></ClientOnly>

完全なベンチマーク行列と履歴の生データは
[外部ダッシュボード](https://celox-sim.github.io/celox/dev/bench/)で確認できます。

## ワークロード

メイン画面にはHeliodorと、SHA-256・FFT・LZ77の代表回路を掲載します。
代表回路は Celox と Veryl simulator の tiered 同士を比較し、主指標は
空のコンパイルキャッシュでのプロセス起動から終了までの時間です。
ricaの既存ランナー、tarunesの検証状況、固定リビジョン、合格条件、次フェーズの
SV frontend比較方針は[回路コレクション](../../benchmarks/collection.md)にまとめています。

[API・カウンタ・標準ライブラリの測定](./diagnostics.md)は診断用として残しています。
コンパイル時間と実行時間は分けて表示します。

[Taxi / cocotb 比較](../../benchmarks/taxi.md)は毎日 03:17 UTC（12:17 JST）に
5構成・合計472テストを Celox と Verilator で実行します。専用タブでテスト本体、
起動込み、HDLコンパイル時間を分けて公開します。

## 結果の読み方

通常の Benchmark workflow は Rust・Verilator・TypeScript を一つのジョブで順番に
実行するため、その run 内では同じ VM・CPU で比較できます。公開する各結果には計測に
使った CPU モデルを記録し、グラフはバックエンドと CPU モデルごとに系列を分けます。
凡例に CPU モデルを表示し、同じバックエンドの別 CPU は線種で区別するため、ランナーの
割り当てが変わったことを性能変化と誤読しません。CPU モデルを記録する前の結果は
`CPU not recorded` として表示します。CPU とランナーの詳細は `bench-host` 成果物に
残します。別 run は異なる CPU に割り当てられる場合があるため、履歴の小さな差だけで
コミットの効果を判断しないでください。
[大規模な Heliodor](./heliodor.md#大規模-linux-バージョン別の測定) も時間内に収まる構成は
同じホストで比較します。ARM の 4 hart は 2 組、8 hart はバックエンド別のジョブに分け、
同じグループ内で同じ CPU を使います。

- 同じワークロード、バックエンド、リビジョン、ホスト環境を比較する。
- 共有 CI ランナー上の小さな差は、再現するまでノイズとして扱う。
- 実行速度は十分に長いワークロードで判断する。
- 開発時の反復時間にはシミュレータ作成時間も含める。
- 最適化設定は実際に使う設計で検証する。

Heliodor では固定入力の追加ワークロードを使います。測定方法は
[Heliodor Linux ベンチマーク](./heliodor.md)を参照してください。
ダッシュボードではネイティブバックエンドと同期版の Veryl-CC を比較し、Celox と
Veryl-CC の tiered 実行では開始までの時間と完了までの総時間も表示します。
Cranelift 単独の起動時間は大幅に長く、この比較用グラフを読みにくくするため掲載しません。
Heliodor のグラフは、異なるランナー間の結果を直接比較しないよう CPU
アーキテクチャ別に分けています。セクション内の系列も同じように CPU モデルごとに
分けます。すべてのグラフの値軸は 0 から始まります。

## ローカル実行

```bash
# CodSpeed によるコンパイル時間ベンチマーク
cargo install cargo-codspeed --locked --version 5.0.1
cargo codspeed build --locked -p celox --bench compilation
cargo codspeed run -p celox

# Rust ベンチマーク
cargo bench -p celox

# TypeScript / N-API ベンチマーク
pnpm bench

# Verilator 比較（Verilator と C++ ツールチェーンが必要）
bash scripts/run-verilator-bench.sh

# VCD 比較（Python 3 も必要。波形の一致を検証してから計測）
python3 scripts/compare-vcd-verilator.py
```

[VCD の測定条件と結果](../../internals/vcd-performance.md#verilator-comparison)には、
idle・sparse・dense の各負荷で、記録なし／ありを比較した結果をまとめています。

CodSpeed ワークフローは毎日 02:17 UTC（11:17 JST）にデフォルトブランチで
実行し、任意のブランチでの手動実行にも対応します。pull request、merge queue、
push では実行しません。決定的な CPU simulation の測定結果を CodSpeed に送信し、
リポジトリの CodSpeed 回帰しきい値に従って過去の測定と比較します。
`master` と `develop` では、実行失敗や性能解析の失敗時に
`CodSpeed is failing on <branch>` Issue を作成・更新し、次の実行と解析が
成功すると閉じます。解析結果の欠落や未完了も失敗として報告します。
Issue には実行へのリンクと、性能低下の場合は比較結果とベンチマークの詳細を
含めます。ローカル実行ではベンチマーク suite が動作することだけを確認します。

ローカル計測は、同じマシン上で 2 つのリビジョンを比較する場合に最も有効です。
CI 履歴は、単発の小さな差より長期的な傾向の確認に向いています。
