# ベンチマーク

Celox には Rust コア、TypeScript ランタイム、および参照ベースラインとしての Verilator のベンチマークスイートが含まれています。CI は `master` への push ごとにベンチマークを自動実行し、インタラクティブなトレンドダッシュボードを公開します。

## ダッシュボード

<ClientOnly><BenchmarkDashboard /></ClientOnly>

このページでは主要なグラフだけを表示します。メインの順序回路ワークロード、主要なテストベンチ指標、代表的な stdlib ワークロードだけに絞っています。必要な場合は DSE のような Celox 側の系列だけ同じグラフ内に残します。
完全なベンチマーク行列と生データは[外部ダッシュボード](https://celox-sim.github.io/celox/dev/bench/)を見てください。

## 測定対象

### Counter (N=1000)

メインワークロードは **N=1000** 個の並列 32 ビットカウンタインスタンスを持つカウンタモジュール（`Top`）を使用します。現実的なワークロードで JIT パイプライン全体を検証します。Rust、TypeScript、Verilator すべてで同一設計を使用し、直接比較が可能です。

| ベンチマーク | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_top_n1000` | JIT コンパイル | NAPI ビルド | Verilate + C++ コンパイル |
| `simulation_tick_top_n1000_x1` | 単一ティック | 単一ティック | 単一ティック |
| `simulation_tick_top_n1000_x1000000` | 100 万ティック | 100 万ティック | 100 万ティック |
| `testbench_tick_top_n1000_x1` | ティック + 読出 | ティック + 読出 | ティック + 読出 |
| `testbench_tick_top_n1000_x1000000` | 100 万テストベンチサイクル | 100 万テストベンチサイクル | 100 万テストベンチサイクル |
| `testbench_array_tick_top_n1000_x1` | — | 配列 `.at()` 単一 | — |
| `testbench_array_tick_top_n1000_x1000000` | — | 配列 `.at()` 100 万 | — |

### 標準ライブラリモジュール

Veryl 標準ライブラリモジュールのベンチマーク。

**Linear SEC (P=6)** — ハミング単一誤り訂正エンコーダ/デコーダ（57 ビットデータ、63 ビット符号語）。組合せ回路。

| ベンチマーク | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_linear_sec_p6` | JIT コンパイル | NAPI ビルド | Verilate + C++ コンパイル |
| `simulation_eval_linear_sec_p6_x1` | 単一評価 | 単一評価 | 単一評価 |
| `simulation_eval_linear_sec_p6_x1000000` | 100 万評価 | 100 万評価 | 100 万評価 |
| `testbench_eval_linear_sec_p6_x1000000` | 100 万評価 + 訂正フラグ読出 | 100 万評価 + 訂正フラグ読出 | 100 万評価 + 訂正フラグ読出 |

**Countones (W=64)** — 再帰的組合せポップカウントツリー。

| ベンチマーク | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_countones_w64` | JIT コンパイル | NAPI ビルド | Verilate + C++ コンパイル |
| `simulation_eval_countones_w64_x1` | 単一評価 | 単一評価 | 単一評価 |
| `simulation_eval_countones_w64_x1000000` | 100 万評価 | 100 万評価 | 100 万評価 |

**std::counter (WIDTH=32)** — マルチモード アップ/ダウンカウンタ（ラップアラウンド付き）。

| ベンチマーク | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_std_counter_w32` | JIT コンパイル | NAPI ビルド | Verilate + C++ コンパイル |
| `simulation_tick_std_counter_w32_x1` | 単一ティック | 単一ティック | 単一ティック |
| `simulation_tick_std_counter_w32_x1000000` | 100 万ティック | 100 万ティック | 100 万ティック |
| `testbench_tick_std_counter_w32_x1000000` | 100 万ティック + 読出 | 100 万ティック + 読出 | 100 万ティック + 読出 |

**std::gray_counter (WIDTH=32)** — Gray エンコード付きカウンタ（counter + gray_encoder）。

| ベンチマーク | Rust | TS | Verilator |
|---|---|---|---|
| `simulation_build_gray_counter_w32` | JIT コンパイル | NAPI ビルド | Verilate + C++ コンパイル |
| `simulation_tick_gray_counter_w32_x1` | 単一ティック | 単一ティック | 単一ティック |
| `simulation_tick_gray_counter_w32_x1000000` | 100 万ティック | 100 万ティック | 100 万ティック |
| `testbench_tick_gray_counter_w32_x1000000` | 100 万ティック + 読出 | 100 万ティック + 読出 | 100 万ティック + 読出 |

**std::fifo (WIDTH=8, DEPTH=16)** — 同期 FIFO（コントローラ + RAM）。順序回路。

| ベンチマーク | Rust | Verilator |
|---|---|---|
| `simulation_build_fifo_w8_d16` | JIT コンパイル | Verilate + C++ コンパイル |
| `simulation_tick_fifo_w8_d16_x1` | 単一ティック（push/pop 交互） | 単一ティック（push/pop 交互） |
| `testbench_tick_fifo_w8_d16_x1000000` | 100 万ティック + 読出 | 100 万ティック + 読出 |

**std::gray_encoder + gray_decoder (WIDTH=32)** — Gray エンコード/デコードラウンドトリップ。組合せ回路。

| ベンチマーク | Rust | Verilator |
|---|---|---|
| `simulation_build_gray_codec_w32` | JIT コンパイル | Verilate + C++ コンパイル |
| `simulation_eval_gray_codec_w32_x1` | 単一評価 | 単一評価 |
| `simulation_eval_gray_codec_w32_x1000000` | 100 万評価 | 100 万評価 |

**std::edge_detector (WIDTH=32)** — ビット単位エッジ検出（posedge/negedge）。順序回路。

| ベンチマーク | Rust | Verilator |
|---|---|---|
| `simulation_build_edge_detector_w32` | JIT コンパイル | Verilate + C++ コンパイル |
| `simulation_tick_edge_detector_w32_x1` | 単一ティック | 単一ティック |
| `testbench_tick_edge_detector_w32_x1000000` | 100 万ティック + 読出 | 100 万ティック + 読出 |

**std::onehot (W=64)** — ワンホット検出・ゼロ検出。組合せ回路。

| ベンチマーク | Rust | Verilator |
|---|---|---|
| `simulation_build_onehot_w64` | JIT コンパイル | Verilate + C++ コンパイル |
| `simulation_eval_onehot_w64_x1` | 単一評価 | 単一評価 |
| `simulation_eval_onehot_w64_x1000000` | 100 万評価 | 100 万評価 |

**std::lfsr_galois (SIZE=32)** — ガロアモード線形帰還シフトレジスタ。順序回路。

| ベンチマーク | Rust | Verilator |
|---|---|---|
| `simulation_build_lfsr_w32` | JIT コンパイル | Verilate + C++ コンパイル |
| `simulation_tick_lfsr_w32_x1` | 単一ティック | 単一ティック |
| `simulation_tick_lfsr_w32_x1000000` | 100 万ティック | 100 万ティック |
| `testbench_tick_lfsr_w32_x1000000` | 100 万ティック + 読出 | 100 万ティック + 読出 |

### API & オーバーヘッド

| ベンチマーク | 説明 |
|---|---|
| `simulator_tick_x10000` | 生の Simulator::tick オーバーヘッド（Rust & TS） |
| `simulation_step_x20000` | Simulation::step 時間ベース API オーバーヘッド（Rust & TS） |

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
