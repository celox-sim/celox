# celox CLIでcocotbを使う

CeloxはVerylまたはSystemVerilogの設計を、VPI経由で通常の
[cocotb](https://www.cocotb.org/) テストを実行できるネイティブ実行ファイルへ
コンパイルできます。生成物にはコンパイル済み設計とCeloxランタイムが含まれるため、
実行時にHDLソースは不要です。

## CLIをビルドする

現在、CLIはCeloxのソースツリーからビルドします。

```sh
cargo build --release -p celox-vpi --features systemverilog --bin celox
```

後述の`run`コマンドにはPythonは不要です。VPIテストを使う場合は、利用するPython環境へcocotbをインストールします。

```sh
python3 -m pip install cocotb
```

リポジトリのdevcontainerにはcocotb 2.0.1とVerilatorがあらかじめ入っています。

## HDLテストベンチを直接実行する

SystemVerilogのテストベンチとVerylのnative `#[test]`モジュールを、同じコマンドで実行できます。

```sh
target/release/celox run src/types.sv src/Dut.sv test/Top.sv --top Top
target/release/celox run test/Top.veryl --top Top
```

SVは`.sv`または`.v`をコンパイル順に指定します。既定では2値シミュレーションを使い、
`$finish`または実行待ちイベントがなくなるまで実行します。
X/Zを保持する場合は`--four-state`または`celox.toml`の`four_state = true`を指定します。
Verylはプロジェクト内の1ファイルを指定すると、`Veryl.toml`から
ソースと依存関係を収集します。単独のVerylファイルも使用でき、Verylも既定は2値です。
`--top`にはnative testbenchのモジュール名を指定します。
既存のnative testbench実行器を使うため、initialブロックの末尾への到達でも完了します。

実行できる構文は既存フロントエンドの対応範囲に従い、未対応構文にはコンパイル診断を返します。
`-O O0`、`-O O1`、`-O O2`で既存コンパイラの最適化レベルを選択できます。
両言語の既定値は不要なストアの削除を含むO2で、HDL testbenchが読む信号は自動的に保持します。
`-O`を明示すると、その指定を優先します。

正常終了は終了コード0、コンパイル・実行エラー、アサーション失敗、`$error`、
`$fatal`、実行時イベントの欠落、実行上限による打ち切りは1、引数エラーは2です。
SVの`$stop`も、対話デバッガーを持たないため終了コード1になります。
SVの`$display`/`$write`は標準出力、VerylのHDL出力は既存実行器に従って標準エラー出力に送ります。

`--max-steps N`で正の実行上限を指定できます（既定値1000000）。
SVではスケジューラのステップ数、Verylではtestbenchのtick数です。
Verylでは完了前にtick上限に達すると失敗します。
コンパイルや1ステップ・tick内の処理時間は制限しないため、実時間の制限が必要な場合は
外部のタイムアウト機能を使ってください。1回の実行で言語を混在させることはできません。

SVのtestbenchでは、`+`で始まる位置引数（または繰り返し指定できる`--plusarg +ARG`）はソースではなくplusargです。
`$test$plusargs`と`$value$plusargs`はコマンドラインの順に参照します（全バックエンド対応）。

```sh
celox run tb.v --top tb +iterations=9 --plusarg +trace
```

両言語とも`--backend native|tiered|interpreter|cranelift|wasm`で実行方式を選べます（既定native）。
tieredはインタープリターで開始し、待機中のイベントを保ちながら生成コードへ移行します。
両言語とも`--threads N`（1〜64、既定1）で並列度を指定できます。
`--unlimited`で実行ステップ・tick数の上限を解除します。

```sh
celox run test/Top.sv --top Top --backend tiered --threads 4
celox run --project /path/to/veryl-project --top smoke --backend tiered --threads 4
celox run test/Top.veryl --top Top --four-state
celox run --project /path/to/veryl-project --top smoke --report --unlimited
```

SVのtestbenchでは、実行中に[状態ファイル](checkpoints.md#状態ファイル)を保存して
`celox state dump`/`celox state diff`で2つの実行やシミュレーターを比較できます（全バックエンド対応）。
`--save-state-on SIGNAL`で数える信号（`tb.clk`のような階層パス、トップモジュール名は省略可）を指定し、
`--save-state-at N[,M...]`で保存するサイクル（1から数える）を指定します。
サイクルは、終了時に信号が1で、直前のタイムステップの終了時には1でなかったタイムステップです。
ファイル`<SIGNAL>-<N>.state`はそのタイムステップの全リージョンが終わった直後に書くので、ノンブロッキング代入の更新も含みます。
`--save-state-at-time T[,U...]`は時刻T以前の処理をすべて終え、Tを越えて進む前の状態を`time-<T>.state`に保存します（単位はデザインのグローバル時間精度）。
`--save-state-dir DIR`で出力先を指定します（既定はカレントディレクトリ）。
実行が指定のサイクルに達しなかった場合はエラーです。

```sh
celox run c906.v --top tb --save-state-on tb.clk --save-state-at 1000,2000 --save-state-dir states
```

Veryl用の`--report`は設定・コンパイル時間・実行時間・tiered昇格統計・結果を
機械可読な`CELOX_*`レコードとして標準出力に出します。通常実行では出しません。
Heliodorベンチも専用Celoxバイナリではなく、このCLIを呼びます。
`--report --backend tiered --require-compiled`では生成コードの実行を必須にできます。
通常の短いtiered実行はインタープリターのまま終了しても成功です。

`--project`は`celox.toml`のほか、Verylプロジェクトディレクトリや`Veryl.toml`を受け取ります。
`--source-file PATH`はプロジェクトからの相対パスでVerylソースを選択します（複数指定可）。
native用の`--build-cache-dir DIR`はVeryl.tomlプロジェクトのコンパイルをキャッシュし、
`--compile-only --native-image-output FILE`でイメージを書き出し、
`--native-image-input FILE`でHDLソースなしに実行できます。
`--dump-ir-dir DIR`でIRを保存し、`--dump-ir-and-run`でそのまま実行できます。


## 複数ファイルのプロジェクトを管理する

共通の入力と名前付きターゲットを`celox.toml`に保存できます。

```toml
sources = ["rtl/pkg.sv"]
filelists = ["rtl/design.f"]
include_dirs = ["include"]
default_target = "smoke"

[defines]
SIMULATION = "1"

[diagnostics]
mixed_timescale = "warn"

[targets.smoke]
sources = ["test/Tb.sv"]
top = "Tb"
opt_level = "O1"
max_steps = 1000000

[targets.dut]
top = "Dut"
```

`run`と`vpi build`は同じソース一覧、includeディレクトリ、マクロ、topを読みます。

```sh
celox run --target smoke
celox vpi build --target dut -o build/dut-sim
celox run --project /path/to/celox.toml --target smoke
```

ソースや`-f`を指定しない場合、または`--target`を指定した場合は、現在のディレクトリから
親方向へ`celox.toml`を探索します。`--project FILE`で直接指定することもできます。
TOML内の相対パスは、ターゲット内も含めてTOMLのあるディレクトリを基準にします。
ソースや`-f`だけを指定した場合は、近くのTOMLを自動では読みません。

ターゲットはルート設定を継承します。ソースの順序は、ルートの`sources`、
ルートの`filelists`、ターゲットの`sources`、ターゲットの`filelists`、
CLIのソースと`-f`（引数に指定した順）の順です。各一覧内の順序を保持します。
includeディレクトリも同じ階層順で追加します。マクロは実行全体に適用し、
後の定義が優先されます。CLIの`-D`は最後に適用します。TOMLのマクロ値は文字列です。
設定されたマクロは各ソースに適用し、HDL内のマクロ定義は既存のファイル単位のスコープに従います。
`--top`は選択されたtopを上書きし、`run`の`-O`と`--max-steps`はターゲット・ルートの
実行設定を上書きします。VPIビルドは既存のforce対応コンパイル設定を使用します。

`--target`を省略すると、`default_target`、ルートの`top`、唯一のターゲットの順に選びます。
複数ターゲットがあり既定値もない場合は`--target`が必要です。
単独の`.veryl`エントリーファイルをプロジェクトに指定することもでき、
Veryl側のソース探索には引き続き`Veryl.toml`を使います。

コンパイル時の診断レベルは `[diagnostics]` で設定でき、`[targets.NAME.diagnostics]`、両コマンドの `--diagnostic CODE=allow|warn|error` の順に上書きできます。CLI オプションは繰り返し指定できます。緩和時の動作と使用例は [診断設定](./celox-toml.md#diagnostics) を参照してください。

### ファイル一覧を直接使う

```sh
celox run -f test/all.f --top Tb
celox vpi build -f rtl/design.f --top Dut -o build/dut-sim
```

`.f`には空白区切りのソースパスを記述し、別の一覧やプリプロセッサ設定も指定できます。

```text
# 相対パスはこのファイルの場所を基準にします。
../rtl/pkg.sv
-f ../rtl/design.f
+incdir+../include
+define+SIMULATION=1
"../test/with spaces/Tb.sv"
```

対応するオプションは`-f FILE`、`-I DIR`/`-IDIR`、
`-D NAME[=VALUE]`/`-DNAME[=VALUE]`、`+incdir+DIR[+DIR...]`、
`+define+NAME[=VALUE][+NAME[=VALUE]...]`です。値のないマクロは`1`になります。
パスには引用符やバックスラッシュのエスケープを使えます。
トークンの先頭にある`#`・`//`による行コメントと、`/* ... */`コメントに対応します。
ネストした一覧の相対パスは、それぞれの一覧があるディレクトリを基準にします。
環境変数・ワイルドカード・シェルの展開は行いません。
未対応オプション、未知のTOMLフィールド、一覧の循環参照はエラーにします。
CLIからも`-I`/`--include-dir`と`-D`/`--define`を直接指定できます。
include・マクロ指定にはSV入力と`--features systemverilog`によるビルドが必要です。

## 設計をコンパイルする

Verylプロジェクト内のソースを1つ指定し、トップレベルモジュール名を渡します。

```sh
target/release/celox vpi build src/Top.veryl --top Top -o build/top-sim
```

Celoxはソースの位置から`Veryl.toml`を探し、プロジェクトのソースと依存関係を
まとめてコンパイルします。Verylプロジェクト外の単独ソースを指定した場合は、
そのファイルだけをコンパイルします。`-o`を省略した出力先は`celox.out`です。

SystemVerilogでは1つ以上の`.sv`ファイルをコンパイル順に指定します。

```sh
target/release/celox vpi build src/types.sv src/Child.sv src/Top.sv --top Top -o build/top-sim
```

Celoxが対応するSystemVerilogの範囲で、指定したファイルをコンパイルします。
package定義は利用側より先に指定してください。`Veryl.toml`や追加のSystemVerilog
ソースは自動探索しません。複数指定時はすべて`.sv`ファイルにする必要があります。
Verylではプロジェクトの入口となるファイルを1つ指定します。
`celox-vpi-compile`も同じソース一覧を受け付けます。

### プロセスを含む設計

遅延やシステムタスクを含む`initial`ブロック、遅延やイベント制御を含む`always`
ブロック、`always #5 clk = ~clk`のようなクロック生成など、プロセスを含む
SystemVerilog設計は時間付きシミュレーションとしてコンパイルされます。生成した
実行ファイルはそのプロセスをcocotbと歩調を合わせて実行します。cocotbのタイマーと
設計の遅延は同じ時間軸を共有し、設計の`$display`/`$write`出力と`$finish`は
`celox run`と同様に動作します。プロセスを含まない設計は従来どおりコンパイル
されます。

1つのタイムステップの中で、cocotbが待つVPIコールバックは次の時点で実行されます。

| cocotbのトリガー(VPIコールバック) | 実行される時点 |
|---|---|
| `Timer`(`cbAfterDelay`)、`NextTimeStep`(`cbNextSimTime`) | タイムステップの開始時、設計のプロセスより前 |
| `RisingEdge`、`FallingEdge`、`Edge`(`cbValueChange`) | 変化の後の次の区切り。ラウンドのプロセスが実行された後で、そのフリップフロップがノンブロッキング更新を行う前。ノンブロッキング更新の各ウェーブとそれに続く組み合わせ回路の評価の後。またはタイムステップが落ち着いた後 |
| `ReadWrite`(`cbReadWriteSynch`) | タイムステップが落ち着いた後。ここでの書き込みは同じ時刻に再び落ち着かせます |
| `ReadOnly`(`cbReadOnlySynch`) | タイムステップの終わり。ここでの書き込みは拒否されます |

したがって、クロックやリセットのエッジは、cocotbと設計のどちらが駆動しても、
それが起動するフリップフロップの確定より前に通知されます。変化は、それを起こした
ウェーブの区切りで通知されます。そのウェーブの組み合わせ回路の評価の後なので、
レジスタを読む組み合わせ回路はすでに新しい値を示し、その変化で起こされるプロセスは
まだ実行されていません。タイムステップ内で設計自身のイベントに対してコールバックを
どこで実行するかはこの実装の選択です。IEEE 1800-2023 4.7はこれらのイベントを処理
する順序をシミュレータに委ねており、たとえばIcarus Verilogはレジスタの変化を、
それを読む組み合わせ回路が追従する前に通知します。同じ理由で、1つのプロセスでの
`x = 1; x = 0;`のような1ウェーブ内のパルスは通知されません。コールバックが見るのは
区切りでの値です。

cocotbからの書き込みは現在時刻のブロッキング代入です。タイムステップの領域の外、
たとえば`Timer`コールバックの中での読み出しは格納された値を返します。同じコール
バックでの書き込みから組み合わせ回路が導く値は、タイムステップが落ち着くまで
以前の値のままです。

プロセスを含む設計に対応しているのは`celox vpi build`だけです。`celox-vpi-compile`
とその`celox-vpi-runtime`は引き続きこれらを拒否します
([#1983](https://github.com/celox-sim/celox/issues/1983))。

実行ファイルには対話モードがないため、`$stop`は`vpiStop`と同様に実行を終了します。
一時停止への対応は[#1886](https://github.com/celox-sim/celox/issues/1886)で
扱っています。

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
