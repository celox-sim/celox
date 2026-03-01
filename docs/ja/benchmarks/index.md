# ベンチマーク

Celox には Rust コア、TypeScript ランタイム、および参照ベースラインとしての Verilator のベンチマークスイートが含まれています。CI は `master` への push ごとにベンチマークを自動実行し、インタラクティブなトレンドダッシュボードを公開します。

## ダッシュボード

<ClientOnly><BenchmarkDashboard /></ClientOnly>

生データは[外部ダッシュボード](https://celox-sim.github.io/celox/dev/bench/)でも確認できます。

## 測定対象

すべてのベンチマークは **N=1000** 個の並列 32 ビットカウンタインスタンスを持つカウンタモジュール（`Top`）を使用します。現実的なワークロードで JIT パイプライン全体を検証します。

### Rust ベンチマーク（Criterion）

| ベンチマーク | 説明 |
|---|---|
| `simulation_build_top_n1000` | JIT コンパイル時間 |
| `simulation_tick_top_n1000_x1` | 単一クロックティック |
| `simulation_tick_top_n1000_x1000000` | 100 万ティックのループ |
| `testbench_tick_top_n1000_x1` | 単一テストベンチサイクル（書込 + ティック + 読出） |
| `testbench_tick_top_n1000_x1000000` | 100 万テストベンチサイクル |
| `simulator_tick_x10000` | 生の Simulator::tick、1 万回反復 |
| `simulation_step_x20000` | Simulation::step、2 万ステップ |

### TypeScript ベンチマーク（Vitest）

| ベンチマーク | 説明 |
|---|---|
| `simulation_build_top_n1000` | JS ビルド / JIT コンパイル時間 |
| `simulation_tick_top_n1000_x1` | 単一ティック |
| `simulation_tick_top_n1000_x1000000` | 100 万ティックのループ |
| `testbench_tick_top_n1000_x1` | 単一テストベンチサイクル |
| `testbench_tick_top_n1000_x1000000` | 100 万テストベンチサイクル |
| `testbench_array_tick_top_n1000_x1` | 配列 `.at()` アクセス付き単一サイクル |
| `testbench_array_tick_top_n1000_x1000000` | 配列 `.at()` アクセス付き 100 万サイクル |

### Verilator（C++ リファレンス）

| ベンチマーク | 説明 |
|---|---|
| `simulation_build_top_n1000` | Verilate + C++ コンパイル時間 |
| `simulation_tick_top_n1000_x1` | 単一クロックティック |
| `simulation_tick_top_n1000_x1000000` | 100 万ティックのループ |
| `testbench_tick_top_n1000_x1` | 単一サイクル（ティック + 出力読出） |
| `testbench_tick_top_n1000_x1000000` | 100 万サイクル（ティック + 出力読出） |

Rust、TypeScript、Verilator のベンチマークは同じ設計を使用しているため、3 つすべての性能を直接比較できます。

## ローカル実行

### Rust

```bash
cargo bench -p celox
```

### TypeScript

```bash
pnpm bench
```

リリースモードで NAPI アドオンをビルドし、パッケージをビルドした後、Vitest ベンチマークを実行します。

### Verilator

```bash
bash scripts/run-verilator-bench.sh
```

`verilator` と C++ ツールチェーンが必要です。

## CI 環境

ベンチマークは GitHub Actions の共有ランナー（`ubuntu-latest`）で実行されます。共有ハードウェアのため、結果にノイズが含まれることがあります。誤検知を避けるため、アラート閾値は 200% に設定されています。個々のデータポイントではなく、長期的なトレンドに注目してください。
