# Heliodor マクロベンチマーク

Heliodor は Veryl で書かれた大規模な RISC-V プロセッサで、Linux boot の ignored test を持っています。プロジェクト読み込み、`$readmemh` による大きなメモリ初期化、native testbench scheduling、長時間の順序回路シミュレーションをまとめて踏むので、Celox を Veryl native simulator として見るためのマクロベンチに向いています。

このベンチは通常 CI には含めません。Heliodor を `target/heliodor/source` に checkout し、`veryl` が無い場合は `target/heliodor/tools` に自動インストールし、Celox runner を測定前に build します。デフォルトでは Veryl baseline を先に測ってから Celox を走らせ、TSV サマリとフルログを `target/heliodor/results` に出します。

## 実行

```bash
scripts/run-heliodor-bench.sh prepare
scripts/run-heliodor-bench.sh run
```

デフォルトでは `test_soc_linux_boot` を Veryl Cranelift、Veryl cc、Celox の順に走らせます。Celox はその test で成功した最速 Veryl baseline の `HELIODOR_CELOX_TIMEOUT_MULTIPLIER` 倍で timeout します。

```bash
HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" \
HELIODOR_RUNNERS="celox veryl-cranelift veryl-cc" \
scripts/run-heliodor-bench.sh run
```

`HELIODOR_REF` を指定しない限り、Heliodor は commit `7ad830fc0f8506c934b61a853ce2eadfa5926b82` に固定します。

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

## 現状の注意

このマクロベンチで Celox 側が記録するのは、現時点では合否と wall-clock time です。Heliodor は `$display` で simulated cycle count を出しますが、Celox の detailed test runner は display event を捨てているため、cycle 抽出は Celox runner が display forwarding を持つまで Veryl runner のログ側だけで利用できます。
