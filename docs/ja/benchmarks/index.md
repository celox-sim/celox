# ベンチマーク

Celox は、各バックエンドおよび Verilator との間でコンパイル時間とシミュレーション
速度を継続的に計測しています。ダッシュボードは傾向の確認に使うもので、すべての
RTL 設計の性能を予測するものではありません。

## ダッシュボード

<ClientOnly><BenchmarkDashboard /></ClientOnly>

完全なベンチマーク行列と履歴の生データは
[外部ダッシュボード](https://celox-sim.github.io/celox/dev/bench/)で確認できます。

## ワークロード

| グループ | 測定対象 |
|---|---|
| コンパイル時間 (CodSpeed) | フロントエンド、最適化、レイアウト、native/Cranelift コード生成の全工程 |
| Counter | 順序状態の更新とクロックイベントのオーバーヘッド |
| 標準ライブラリ | 組み合わせ回路、順序回路、構造化データパス |
| TypeScript テストベンチ | N-API 呼び出し、型付き信号アクセス、スケジューラ |
| Verilator 比較 | 同等の生成シミュレータによる基準値 |
| Heliodor Linux | 大規模な外部設計での生成コード実行速度 |

コンパイルと実行は分けて報告します。コンパイルが速くても生成コードが速いとは
限らず、マイクロベンチマークだけで設計全体の性能は判断できません。

## 結果の読み方

- 同じワークロード、バックエンド、リビジョン、ホスト環境を比較する。
- 共有 CI ランナー上の小さな差は、再現するまでノイズとして扱う。
- 実行速度は十分に長いワークロードで判断する。
- 開発時の反復時間にはシミュレータ作成時間も含める。
- 最適化設定は実際に使う設計で検証する。

Heliodor では固定入力の追加ワークロードを使います。測定方法は
[Heliodor Linux ベンチマーク](./heliodor.md)を参照してください。

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
```

CodSpeed ワークフローは pull request と `master` でベンチマークを実行します。
CodSpeed は `merge_group` event をサポートしていないため、merge queue では
workflow check を維持しつつ CodSpeed を実行しません。pull request は決定的な CPU
simulation を使って `master` の基準値と比較されます。ローカル実行では
ベンチマーク suite が動作することだけを確認します。

ローカル計測は、同じマシン上で 2 つのリビジョンを比較する場合に最も有効です。
CI 履歴は、単発の小さな差より長期的な傾向の確認に向いています。
