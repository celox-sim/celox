# Heliodor マクロベンチマーク

Heliodor は Veryl で書かれた大規模な RISC-V プロセッサで、Linux boot の ignored test を持っています。プロジェクト読み込み、`$readmemh` による大きなメモリ初期化、native testbench scheduling、長時間の順序回路シミュレーションをまとめて踏むので、Celox を Veryl native simulator として見るためのマクロベンチに向いています。

このベンチは通常 CI には含めません。Heliodor を `target/heliodor/source` に checkout し、`veryl` が無い場合は `target/heliodor/tools` に自動インストールし、Celox runner を測定前に build します。デフォルトでは Veryl baseline を先に測ってから Celox を走らせ、TSV サマリとフルログを `target/heliodor/results` に出します。

## 実行

```bash
scripts/run-heliodor-bench.sh prepare
scripts/run-heliodor-bench.sh run
```

`run` は構成を変更できる診断用コマンドです。測定値を追記しますが、Celox
が性能要件を満たすかは判定しません。その判定には固定構成の `gate` を使います。

デフォルトでは `test_soc_linux_boot` を Veryl Cranelift、Veryl cc、Celox の順に走らせます。Celox はその test で成功した最速 Veryl baseline の `HELIODOR_CELOX_TIMEOUT_MULTIPLIER` 倍で timeout します。

```bash
HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" \
HELIODOR_RUNNERS="celox veryl-cranelift veryl-cc" \
scripts/run-heliodor-bench.sh run
```

`HELIODOR_REF` を指定しない限り、Heliodor は commit `7ad830fc0f8506c934b61a853ce2eadfa5926b82` に固定します。

## Acceptance gate

clean かつ commit 済みの Celox checkout から、再現可能な end-to-end 比較を
実行します。

```bash
scripts/run-heliodor-bench.sh gate
```

gate の構成は変更できません。以下をすべて固定します。

- 公式 repository の Heliodor commit
  `7ad830fc0f8506c934b61a853ce2eadfa5926b82` と clean な checkout
- benchmark 所有ディレクトリにある Veryl `0.20.2`。`PATH` や
  `VERYL_BIN` は使わず、固定 path と `--version` の完全一致で選択
- clean で途中に変化しない Celox `HEAD`。invocation ごとの空の Cargo
  target directory に `--locked` release build し、その成果物を実行
- `test_soc_linux_boot`、runner 順序 `veryl-cc`、`celox`、各 300 秒 timeout
- Celox native backend、`O2`、2-state、full execution、SIR pass override なし
- runner ごとに別の detached Heliodor worktree。project-local な生成物を
  runner 間で共有しない

gate は `target/heliodor/results` の下に、新しい独立した
`gate_<timestamp>.<suffix>` directory を作ります。その invocation が生成した
2 行だけを受理します。Veryl は正常終了し、対象 test の成功をちょうど 1 件、
集計を `1 passed, 0 failed` と報告する必要があります。Celox は正常終了し、
native/O2/`four_state=false`/`compile_only=false` の config 行と full-pass
result 行をそれぞれちょうど 1 件報告する必要があります。実行前後で source
manifest、checkout identity、runner executable hash も検査します。

subprocess 時間は monotonic nanosecond clock で測定します。両方の semantic
check が成功し、Celox の process 時間が Veryl 以下の場合にだけ gate は 0 で
終了します。compile-only、partial window、runner 内部の報告時間、または正確な
marker を伴わない process exit 0 は失敗です。`--kill-after` を持つ GNU
`timeout` と Python 3 が必要です。

Celox は現時点で、この gate における競争力のある full Linux-boot 成功結果を
一度も出していません。コマンドの実装によって acceptance 判定を実行可能にした
のであって、それ自体が性能要件の合格実績になるわけではありません。

## テスト

Heliodor の `#[test]` module 一覧は以下で見られます。

```bash
scripts/run-heliodor-bench.sh list
```

主な長時間テスト:

| Test | 意味 |
|---|---|
| `test_soc_linux_boot` | Linux 5.15 single-hart boot |
| `test_soc_smp_linux_boot_2hart` | Linux 5.15 SMP 2-hart boot |
| `test_soc_smp_linux_boot_4hart` | Linux 5.15 SMP 4-hart boot |
| `test_soc_linux_boot_71` | Linux 7.1 single-hart boot |
| `test_soc_smp_linux_boot_71_2hart` | Linux 7.1 SMP 2-hart boot |
| `test_soc_linux_boot_71v` | Linux 7.1 vector-enabled boot |

## ランナー

`HELIODOR_RUNNERS` には以下を指定できます。

| Runner | Command |
|---|---|
| `celox` | `target/release/examples/run_veryl_project_test --project ... --test ...` |
| `veryl-cc` | `veryl test --ignored --test ... --backend cc` |
| `veryl-cranelift` | `veryl test --ignored --test ... --backend cranelift` |
| `veryl-interpret` | `veryl test --ignored --test ... --backend interpret` |

Celox runner は Celox の default backend を使います。x86-64 host では native x86-64 backend です。最適化プリセットは `CELOX_OPT_LEVEL=O0|O1|O2` で変えられます。

全 runner/test の timeout を固定したい場合は `HELIODOR_TIMEOUT_SEC` を指定します。Veryl baseline がまだない場合、Linux boot は single-hart 300 秒、2-hart SMP 600 秒、4-hart SMP 1800 秒などの固定 fallback を使います。

`veryl` が `PATH` に無い場合、スクリプトは `cargo install veryl --version 0.20.2 --locked` を `target/heliodor/tools/veryl-0.20.2` に実行します。`VERYL_BIN`、`HELIODOR_VERYL_VERSION` で上書きできます。自動インストールを止める場合は `HELIODOR_INSTALL_TOOLS=0` を指定します。

## 結果の意味

`target/heliodor/results/results.tsv` は subprocess の終了 status と、
シミュレーションした test の意味上の結果を区別します。列は以下です。

| 列 | 意味 |
|---|---|
| `runner` | runner 名 |
| `test` | 指定した Heliodor test |
| `status` | 既存 reader のため第 3 列に残す `exit_status` の旧名 |
| `elapsed_ns` | full pass の wall time。full pass 以外は必ず `NA` |
| `log` | runner の完全な log |
| `semantic_status` | `pass`、`fail`、`compile-only`、`unreported`、`invalid` |
| `exit_status` | subprocess の終了 status |
| `process_elapsed_ns` | fail や compile-only を含む subprocess の monotonic elapsed time |
| `reported_elapsed_ns` | Celox runner 内部の elapsed。取得できない場合は `NA` |

従来の `runner`、`test`、`status`、`elapsed_ns`、`log` は同じ順序で
残ります。速度結果として扱えるのは `semantic_status=pass`、
`exit_status=0` かつ `elapsed_ns` が数値の行だけです。
`process_elapsed_ns` と `reported_elapsed_ns` は診断値であり、
`compile-only`、`fail`、`unreported`、`invalid` の full-test 性能として
扱ってはいけません。

Celox については、ログ中に次の完全な行がちょうど 1 個必要です。

```text
CELOX_TEST_RESULT test=<requested-test> status=pass|fail|compile-only elapsed_ns=<integer>
```

形式不正、重複、欠落、test 名の不一致、compile-only mode との不一致、
process 終了 status との不一致は pass になりません。
`HELIODOR_CELOX_COMPILE_ONLY=1` が正常終了しても、`semantic_status` は
`compile-only`、`elapsed_ns` は `NA` です。

既存の 5 列 TSV は次回実行時に atomic に移行します。最初の内容を
`results.tsv.v1.bak` に保存し、参照先の Celox log から意味上の結果を
復元できない行は `unreported` または `invalid` にします。process の
終了 status が 0 という事実だけで Celox full pass に昇格させることは
ありません。

parser/migration と acceptance gate の fixture は、Heliodor や compiler を
checkout・実行せずにテストできます。

```bash
bash scripts/tests/run-heliodor-bench-results.sh
bash scripts/tests/run-heliodor-bench-gate.sh
```

## 現状の注意

このマクロベンチで Celox 側が記録するのは、現時点では意味上の結果、process の結果、wall-clock time です。Heliodor は `$display` で simulated cycle count を出しますが、Celox の detailed test runner は display event を捨てているため、cycle 抽出は Celox runner が display forwarding を持つまで Veryl runner のログ側だけで利用できます。
