# Heliodor マクロベンチマーク

Heliodor は Veryl で書かれた大規模な RISC-V プロセッサで、Linux boot の ignored test を持っています。プロジェクト読み込み、`$readmemh` による大きなメモリ初期化、native testbench scheduling、長時間の順序回路シミュレーションをまとめて踏むので、Celox を Veryl native simulator として見るためのマクロベンチに向いています。

このベンチは通常 CI には含めません。Heliodor を
`target/heliodor/source` に checkout し、計測前に時間分離版 Veryl runner と
Celox runner を build します。デフォルトでは Veryl baseline を先に測ってから
Celox を走らせ、TSV サマリとフルログを `target/heliodor/results` に出します。

## 実行

```bash
scripts/run-heliodor-bench.sh prepare
scripts/run-heliodor-bench.sh run
```

`run` は構成を変更できる診断用コマンドです。測定値を追記しますが、Celox
が性能要件を満たすかは判定しません。その判定には固定構成の `gate` を使います。

デフォルトでは `test_soc_linux_boot` を同期 Veryl AOT-C、Celox の順に走らせます。
Celox はその test で成功した Veryl baseline の
`HELIODOR_CELOX_TIMEOUT_MULTIPLIER` 倍で timeout します。

```bash
HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" \
HELIODOR_RUNNERS="celox veryl-cranelift veryl-cc" \
scripts/run-heliodor-bench.sh run
```

`HELIODOR_REF` を指定しない限り、Heliodor は commit `7ad830fc0f8506c934b61a853ce2eadfa5926b82` に固定します。

## コード生成時間と実行時間の分離

デフォルトの同期 Veryl-CC runner は、生成コードの throughput と compiler
latency を分離します。

```bash
HELIODOR_RUNNERS="veryl-cc-sync celox" \
HELIODOR_TIMEOUT_SEC=300 \
scripts/run-heliodor-bench.sh run
```

両 runner は build/prepare と testbench 実行を別々に報告します。compile 区間には
frontend 解析、最適化、native コード生成、Simulator 初期化、initial testbench
lowering を含み、構築済みtestbenchを実行する直前で終了します。
Veryl 側は Veryl 0.20.2 の Heliodor benchmark と同じ同期 AOT-C 設定
（`aot_c_async=false`）を使うため、コード生成完了後に実行が始まります。
これにより、入力依存の Cranelift から C への hot-swap 時点を実行時間に混ぜません。
各 Veryl-CC 実行には新しい空の一時 AOT cache を割り当てるため、共有 cache の
`.so` hit をコード生成時間として誤計測しません。source 読み込みと process 起動は
両内部区間の外で、`process_elapsed_ns` にだけ含まれます。

最初の非 LTO 分離測定は、両者とも正確な
`cy=9ae070 x3=aa pass=1` で完了しました。

| Runner | コード生成 | 実行 |
|---|---:|---:|
| `veryl-cc-sync` | 58.354 s | 54.282 s |
| `celox` | 40.450 s | 137.675 s |

したがって現時点の生成コード実行差は `2.536x` です。この測定では Celox の
cold compile 区間は Veryl の `0.693x` でした。従来の固定 gate の `2.605x` は
end-to-end process 比であり、実行だけの比ではありません。native 実行最適化の
採否には `execute_elapsed_ns`、compiler latency には `compile_elapsed_ns` を
使います。この記録は
`target/heliodor/results/split_timing_aligned_20260716T021500Z` にあります。

## Acceptance gate

clean かつ commit 済みの Celox checkout から、再現可能な throughput 比較を
実行します。

```bash
scripts/run-heliodor-bench.sh gate
```

gate の構成は変更できません。以下をすべて固定します。

- 公式 repository の Heliodor commit
  `7ad830fc0f8506c934b61a853ce2eadfa5926b82` と clean な checkout
- clean な Celox `Cargo.lock` と locked build で固定した Veryl simulator
  `0.20.2`。`PATH` や `VERYL_BIN` の CLI は使わない
- clean で途中に変化しない Celox `HEAD`。invocation ごとの空の Cargo target
  directory に時間分離版 Veryl と Celox の locked release/LTO build を行い、
  その成果物を実行
- `test_soc_linux_boot`、runner 順序 `veryl-cc-sync`、`celox`、各 300 秒 timeout
- invocation ごとに新しい空の Veryl AOT cache を作り、実行後に削除
- Celox native backend、`O2`、2-state、full execution、SIR pass override なし
- runner ごとに別の detached Heliodor worktree。project-local な生成物を
  runner 間で共有しない

gate は `target/heliodor/results` の下に、新しい独立した
`gate_<timestamp>.<suffix>` directory を作ります。その invocation が生成した
2 行だけを受理します。両 runner は正常終了し、正確な config 行、分離 timing 行、
full-pass result 行をそれぞれちょうど 1 件報告する必要があります。Celox は
native/O2/`four_state=false`/`compile_only=false`、Veryl は同期 AOT-C の
`aot_c_async=false` でなければなりません。実行前後で source manifest、
checkout identity、runner executable hash も検査します。
さらに両ログに architectural completion marker がちょうど 1 件あり、
16 進数の先頭 0 を除いて `cy=9ae070 x3=aa pass=1` と一致することを要求します。

両 runner は Simulator と initial testbench の構築完了までを
`compile_elapsed_ns`、構築済みtestbenchの実行だけを `execute_elapsed_ns` として
測ります。両方の semantic check が成功し、Celox の実行区間が Veryl 以下の場合に
だけ gate は 0 で終了します。コード生成 latency は別に記録・表示し、実行
throughput の判定には混ぜません。subprocess 時間は end-to-end の診断値です。
compile-only、partial window、または正確な marker を伴わない process exit 0 は
失敗です。`--kill-after` を持つ GNU `timeout` と Python 3 が必要です。

時間分離前の直近の反復用非 LTO 比較では、同じ `cy=9ae070` の workload が
Veryl-CC で `76.446 s`、Celox で `184.652 s` でした。その後、clean な Celox
commit `e917489e` から fresh な locked release/LTO runner を build して固定 gate
を実行しました。Veryl-CC は `68.409 s`、Celox は process 時間 `178.223 s`
（runner 内部報告 `178.019 s`）でした。両者の semantic check は成功し、両ログに
`cy=9ae070 x3=aa pass=1` がちょうど 1 件ありましたが、Celox は `2.605x`
遅いため、当時の合計process時間による性能条件に失敗しました。成果物は
`target/heliodor/results/gate_20260716T010312Z.tcVUZd` にあります。通常の開発反復
では引き続き非 LTO の `heliodor-dev` profile を使います。分離後の固定 gate は、
実装をcommitし、意図した最終release/LTO qualificationを行う段階でだけ実行します。

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
| `celox` | `target/<profile>/examples/run_veryl_project_test --project ... --test ...` |
| `veryl-cc-sync` | 分離計測を行う Veryl 0.20.2 同期 AOT-C runner |
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
| `reported_elapsed_ns` | runner 内部の総 elapsed。取得できない場合は `NA` |
| `compile_elapsed_ns` | Simulator・testbench 構築までを含む build/prepare 内部時間。取得できない場合は `NA` |
| `execute_elapsed_ns` | コード生成完了後の testbench 実行時間。取得できない場合は `NA` |

従来の `runner`、`test`、`status`、`elapsed_ns`、`log` は同じ順序で
残ります。end-to-end process 結果として扱えるのは `semantic_status=pass`、
`exit_status=0` かつ `elapsed_ns` が数値の行だけです。この旧列は生成コードの
実行性能ではありません。throughput には `execute_elapsed_ns`、compiler latency
には `compile_elapsed_ns` を使います。`process_elapsed_ns` と
`reported_elapsed_ns` は診断値であり、
`compile-only`、`fail`、`unreported`、`invalid` の full-test 性能として
扱ってはいけません。

Celox については、ログ中に timing 行と result 行がそれぞれちょうど 1 個必要です。

```text
CELOX_TEST_TIMING test=<requested-test> compile_ns=<integer> execute_ns=<integer>
CELOX_TEST_RESULT test=<requested-test> status=pass|fail|compile-only elapsed_ns=<integer>
```

`veryl-cc-sync` は対応する `VERYL_TEST_TIMING` と `VERYL_TEST_RESULT` を
出力します。Celox の compile-only 結果は `execute_ns=0` でなければならず、
分離区間の合計は runner 内部の総時間を超えてはなりません。

形式不正、重複、欠落、test 名の不一致、compile-only mode との不一致、
process 終了 status との不一致は pass になりません。
`HELIODOR_CELOX_COMPILE_ONLY=1` が正常終了しても、`semantic_status` は
`compile-only`、`elapsed_ns` は `NA` です。

既存の 5 列・9 列 TSV は次回実行時に atomic に移行します。最初の内容を
`results.tsv.v1.bak` または `results.tsv.v2.bak` に保存し、参照先 log から
分離時間を復元できる場合は復元し、取得できない値は `NA` にします。process の
終了 status が 0 という事実だけで Celox full pass に昇格させることはありません。

parser/migration と acceptance gate の fixture は、Heliodor や compiler を
checkout・実行せずにテストできます。

```bash
bash scripts/tests/run-heliodor-bench-results.sh
bash scripts/tests/run-heliodor-bench-gate.sh
```

## Architectural completion marker

Celox の testbench runner は Heliodor の `$display` を転送するため、Celox と
Veryl の両ログに simulated cycle、architectural result register、pass bit が
残ります。固定 gate は process exit や test-result record とは独立に
`cy=9ae070 x3=aa pass=1` を検査します。この検査は必須です。以前の native
ISel 幅バグは `pass=1` のまま power-down しましたが、cycle は `9ab960` でした。
