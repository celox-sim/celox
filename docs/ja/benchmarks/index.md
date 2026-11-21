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
**rica** タブは5種類の自己検証付き RISC-V カーネルを Celox tiered / Veryl simulator tiered で比較します。
毎日 04:47 UTC（13:47 JST）に計測し、コンパイルを含むプロセス総時間の3回の中央値を公開します。
rica の計測条件、tarunesの検証状況、固定リビジョン、合格条件、次フェーズの
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
# CI と同じ CodSpeed ベンチマーク。rtlmeter ベンチマークは CELOX_RTLMETER_LAYOUT に
# 配置したケースを実行し、未設定なら何も実行しない（PyYAML が必要。--optional で
# BlackParrot も追加）
python3 scripts/run-rtlmeter-bench.py prepare work/rtlmeter
python3 scripts/run-rtlmeter-bench.py codspeed-layout work/rtlmeter work/rtlmeter-layout
export CELOX_RTLMETER_LAYOUT=$PWD/work/rtlmeter-layout
cargo install cargo-codspeed --locked --version 5.0.1
cargo codspeed build --locked -p celox --features cranelift --bench compilation --bench scaling --bench rtlmeter
cargo codspeed run -p celox --bench compilation --bench scaling --bench rtlmeter

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

CodSpeed ワークフローは CodSpeed の ARM64 Macro Runner
（`codspeed-macro-arm64-graviton-ubuntu-22-04`）上で walltime 計測を行います。
`master` への push ごと、毎日 02:17 UTC（11:17 JST）、手動実行、`bench` ラベルの
付いた pull request（`bench-blackparrot` では BlackParrot も計測）で実行します。
1 回の実行でランナーを 1〜2 時間使うため、その他の pull request と merge queue では
実行しません。結果はリポジトリの CodSpeed 回帰しきい値に従って過去の測定と比較します。
`master` と `develop` では、実行失敗や性能解析の失敗時に
`CodSpeed is failing on <branch>` Issue を作成・更新し、次の実行と解析が
成功すると閉じます。解析結果の欠落や未完了も失敗として報告します。
Issue には実行へのリンクと、性能低下の場合は比較結果とベンチマークの詳細を
含めます。ローカル実行ではベンチマーク suite が動作することだけを確認します。

| ベンチマーク | 計測内容 | 検出する回帰 |
|---|---|---|
| `compile/*`、`scaling/nested_write/*`、`compile/sorter_tree/*`（`compilation.rs`） | Veryl 設計のビルド時間 | コンパイル時間の悪化、超線形なパス |
| `scaling/clock_alias/{compile,sim}/n{16,1024}`（`scaling.rs`） | それぞれ独自の `assign l1clk = clk` でクロックされる N 個のフロップ。テストベンチは `clk` を negedge で検出（VeeR-EH1 にはこのコピーが 1,451 個ある） | クロックドメイン数に比例して増えるイベントあたりのスケジューラコスト |
| `scaling/gated_clock/{compile,sim}/n{16,1024}` | クロックゲート（negedge で `en_ff` を取り込み、`gclk = clk & en_ff`）の後ろの N 個のフロップ | ゲート・派生クロックあたりのコスト |
| `fallback/<construct>/sim` と `fallback/baseline/sim` | 64 本の組合せレーンに、組合せ `$display`、`assert final`、`always @*` 内の DPI-C 呼び出し、`string` 配列、`final` ブロックのいずれか 1 つを追加 | 1 つの構文で設計全体が遅い経路に移る回帰（settle skip や partitioned settle の無効化、スロットごとの compiled run、reference walk）。いずれも baseline に近いままであるべき |
| `rtlmeter/<case>/compile`（`rtlmeter.rs`） | VeeR-EL2、VeeR-EH1、XuanTie-E906 の CoreMark に対する `celox run` の解析とコード生成（BlackParrot 1x1 は `bench-blackparrot` または nightly） | 実設計でのコンパイル時間の回帰 |
| `rtlmeter/<case>/cycle` | 固定 `+max_cycles` ウィンドウの実行時間をサイクル数で割った値（native、O2、2-state、1 スレッド。プログラムのロードなどテストベンチの起動を含む） | 設計全体を遅くする構文を含む、実設計でのサイクルあたりの回帰 |

シミュレーションベンチマーク（`sim`）は 1,000 クロックサイクルの時間を報告します。
RTLMeter ケースは `benchmarks/rtlmeter/codspeed_manifest.json`（固定した RTLMeter
リビジョン、ウィンドウ、回数）から作ります。各実行はテストベンチの失敗なしに
ウィンドウの終わりまで到達しなければならず、そうでなければベンチマークは失敗します。
常駐メモリが `CELOX_RTLMETER_MEMORY_LIMIT_MIB`（CI では 20 GiB。VeeR-EH1 のピークは約 7 GB）
または時間が `CELOX_RTLMETER_TIMEOUT_SECS` を超えた実行は強制終了され、そのベンチマークは
失敗します。各実行は開始とピークメモリを出力します。ワークフローは CodSpeed の walltime
プロファイラを使いません。プロファイラは celox コマンドを含むすべての計測対象プロセスを
8 KiB のスタックコピー付きで数十分間サンプリングし、ランナー（32 GB、Cortex-A72 16 コア）が
接続を失いました。ビルドと実行の間、メモリとディスクの使用量を毎分出力します
（`scripts/with-resource-log.sh`）。

ローカル計測は、同じマシン上で 2 つのリビジョンを比較する場合に最も有効です。
CI 履歴は、単発の小さな差より長期的な傾向の確認に向いています。

[RTLMeter 比較](./rtlmeter.md)では XuanTie-E902/E906 と HummingbirdV2-E203 の hello を用い、
2-state / 4-state を分けて Verilator・Nix 固定の安定版 xezim と比較します。
コンパイルから実行完了までを測り、未対応や失敗も表に表示します。
