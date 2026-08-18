# celox CLIでcocotbを使う

CeloxはVerylの設計を、VPI経由で通常の
[cocotb](https://www.cocotb.org/) テストを実行できるネイティブ実行ファイルへ
コンパイルできます。生成物にはコンパイル済み設計とCeloxランタイムが含まれるため、
実行時にVerylソースは不要です。

## CLIをビルドする

現在、CLIはCeloxのソースツリーからビルドします。

```sh
cargo build --release -p celox-vpi --bin celox
```

利用するPython環境へcocotbをインストールします。

```sh
python3 -m pip install cocotb
```

リポジトリのdevcontainerにはcocotb 2.0.1とVerilatorがあらかじめ入っています。

## 設計をコンパイルする

Verylプロジェクト内のソースを1つ指定し、トップレベルモジュール名を渡します。

```sh
target/release/celox vpi build src/Top.veryl --top Top -o build/top-sim
```

Celoxはソースの位置から`Veryl.toml`を探し、プロジェクトのソースと依存関係を
まとめてコンパイルします。Verylプロジェクト外の単独ソースを指定した場合は、
そのファイルだけをコンパイルします。`-o`を省略した出力先は`celox.out`です。

## cocotbを実行する

たとえば`test/test_top.py`にcocotbテストがある場合、そのモジュールをimportできる
状態で生成物を起動します。

```sh
PYTHONPATH=test build/top-sim --test-module test_top
```

生成された実行ファイルは自動的に次を行います。

- `python3`からcocotbのVPIアダプターとlibpythonを検出する
- コンパイル済みのトップレベル名をcocotbへ渡す
- `results.xml`を出力し、テスト失敗時は非ゼロで終了する

別のPython環境や結果ファイルを使う場合は明示できます。

```sh
build/top-sim \
  --test-module test_top \
  --python .venv/bin/python \
  --results-file build/results.xml
```

`--test-filter REGEX`で一致するテストに絞れます。cocotbの従来形式向けに
`--testcase NAME`も利用できます。`--vpi PATH`を指定するとVPIアダプターの自動検出を
上書きします。既存の自動化向けに、`PYGPI_PYTHON_BIN`、`LIBPYTHON_LOC`、
`CELOX_COCOTB_VPI`、`COCOTB_TEST_MODULES`、`COCOTB_TEST_FILTER`、
`COCOTB_RESULTS_FILE`も引き続き利用できます。

## 現在の互換範囲

cocotb 2.0で使われるモジュール・信号の探索、immediate/deposit/force/release書き込み、
スカラー・ベクター値、シミュレーション時刻、およびStart、ReadWrite、ReadOnly、
NextTimeStep、Timer、ValueChange、Endの各コールバック領域に対応しています。

packed bit handle、遅延VPI書き込み、unpacked arrayのインデックス参照、
派生・カスケードクロックのスケジューリングにはまだ対応していません。
