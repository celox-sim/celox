---
outline: [2, 3]
---

# SystemVerilog の逸脱事項

導入と最初のシミュレーションは [SystemVerilog サポート](./systemverilog)から始めてください。

## 前提: IEEE 1800-2023 {#what-is-supported}

Celox は IEEE Std 1800™-2023 が規定する SystemVerilog を実装します。
このページは標準をなぞりません。Celox が標準と異なる点だけを挙げます。

1. **未対応**の構文。コンパイル時に拒否します。
2. **意図的な逸脱**。受理するが標準と異なる挙動です。
3. **処理系定義の選択**。標準が処理系に委ねる、または順序を指定しない点です。
4. **Celox の実行モデル**。標準に対応物の無い部分です
   （Rust の `Simulator` / `Simulation` の分離、チェックポイント、
   言語混在、DPI-C 登録）。

ここに無い挙動は標準に従います。
載っていない差異に気づいたら、文書化されていない仕様ではなく不具合として
[issue を登録](https://github.com/celox-sim/celox/issues)してください。
測定した適合状況は[検証結果のページ](/ja/verification/)で公開しています。

未対応の構文は黙って無視せず、構文名を含む `Unsupported` エラーを返します。
エラーには追跡 issue の番号が付きます。個別の issue が無い構文は
フロントエンドのロードマップ [#88](https://github.com/celox-sim/celox/issues/88)を指します。

## 未対応の構文 {#what-is-not-supported}

### デザイン構造

- `bind`、`checker`、`config`、`covergroup`、clocking ブロックと clocking drive、
  ユーザー定義プリミティブ（UDP）、`specify` ブロック。
- 文を持つモジュール自身のモジュールインスタンス（generate ブロック内でも可）の
  パラメータ以外への `defparam` 代入: 子インスタンスを経由する名前、`$root` から
  始まる名前、インスタンス配列の要素、インタフェースインスタンス、文自身の
  スコープのパラメータ。
- ポートとインスタンス: `module m({a, b}, c[3:0], .p(d))` のような non-ANSI ポート式
  （[#1953](https://github.com/celox-sim/celox/issues/1953)。ポート名のヘッダと
  本体の宣言による素の non-ANSI ポートは対応）、`ref` ポート
  （[#427](https://github.com/celox-sim/celox/issues/427)）、SystemVerilog ソースの外で
  定義されたモジュールへのワイルドカード `.*` 接続
  （[#442](https://github.com/celox-sim/celox/issues/442)）、ANSI ポートの既定値。
- 複数ドライバのネット。ただし 1 つのモジュール内の継続代入が `wire`、`tri`、`wand`、
  `triand`、`wor`、`trior` のネットを駆動する場合は解決します（6.6.1〜6.6.3）。
  インスタンスの下にあるドライバ（モジュールをまたぐトライステートバス）、
  駆動強度付きの代入、複数ドライバの `tri0`・`tri1`・supply ネット、および
  2 値シミュレーションで X/Z リテラルを含むドライバ（2 値の値はハイインピーダンスを
  保持できないため）は引き続き拒否します。トップモジュールの `inout` ポートも拒否します。
  トップより下の `inout` は、`assign io = oe ? d : 'z;` のように両側とも
  継続ドライバが1つの場合だけ対応します。ドライバの無い内部ネットは拒否します
  （[#460](https://github.com/celox-sim/celox/issues/460)）。
  ただし独立遅延のあるネットの未駆動ビットは `z` を読みます。
- ゲート・スイッチプリミティブは、遅延・強度の無いスカラ `and`、`nand`、`or`、
  `nor`、`xor`、`xnor` のみ対応します（Clause 28）。ゲートの遅延・強度・
  インスタンス配列も未対応です（[#457](https://github.com/celox-sim/celox/issues/457)）。
- 負の下限を持つインスタンス配列。
- 階層参照 <a id="hierarchical-references"></a>: 継続代入でネットや変数を駆動する参照、
  ポート接続内の参照、task・関数・名前付きブロック・パラメータ・他スコープの
  構造体メンバ（packed 構造体変数のメンバを除く。packed 構造体配列の要素の
  メンバは拒否します）への参照、`real`・`string`（後述のホスト API 経由を除く）・
  クラス型信号への参照、generate ブロック内部から相対的に始まる参照は拒否します。
- package: package ネット（[#1146](https://github.com/celox-sim/celox/issues/1146)）。
  package 変数はクロックやリセットにできません。
- コンパイル単位: `$unit` 変数を宣言できるソースファイルは1つだけです。
- `let` 宣言（11.12）はモジュール・interface・program の項目としてのみ扱い、
  呼び出しごとに展開します。package・generate ブロック・手続きブロック内の
  ものは拒否します。本体の名前を宣言し直すスコープ内の呼び出しも拒否します。

### interface の制約 {#interface-restrictions}

- **宣言:** interface 内の暗黙ネット、enum、モジュールインスタンス、
  マクロ、コンパイラ指令。再帰する特殊化と64段を超える入れ子は拒否します。
- **modport と接続:** `inout`、`ref`、式ポート、`export`、clocking modport。
  受け側ポートが modport を宣言していないのに modport を名指す接続。
  modport の無いポート越しの書き込み。modport の無いポートから non-ANSI
  モジュールへのメンバ接続。
- **関数呼び出し:** 選択していない interface 配列や実行時添字越しの呼び出し。
  interface インスタンス配列の論理からメンバに触れる関数への呼び出し。
  generate・関数・task 内部で、ポート越しにメンバを書く import 関数を呼ぶこと。
- **サブルーチンの static 記憶域:** 複数のモジュールが interface ポート越しに呼ぶ
  static な関数・task の static 変数（中断する task の static な引数を含む）は、
  interface インスタンスごとに一つではなく、呼び出すモジュールごとに持ちます
  （13.3.2、13.4.2、[#3739](https://github.com/celox-sim/celox/issues/3739)）。
  インスタンス自体越しの呼び出しと、一つのモジュールのポート越しの呼び出しは一つを共有します。
- **宣言の依存:** ポート越しに使う interface のパラメータ・定数・型・メンバ宣言での
  メンバ参照。別のファイルのモジュールや interface より前に使うモジュールが
  コンパイル単位の項目を使う場合。
- **名前と階層:** 暗黙の generate 名（`genblk1`）への参照と generate ブロック越しの
  パス（`g.h.x`）。interface インスタンス名・ポート名の再利用。interface 経由で
  取り込んだ package 名と、別の import 済み package の項目やモジュール宣言との衝突。
- **識別子:** interface を使うデザインでは、自分の名前に `$` を使えません。
  単純識別子でないエスケープ識別子も使えません。展開が `$` を生成名用に予約します。
- interface 配列のメンバに対する変数宣言初期化子。
- virtual interface <a id="virtual-interfaces"></a>: 自動変数・引数・クラスプロパティに保持した
  virtual interface 越しの待ち。パラメータ付き interface の virtual interface。
  virtual interface 越しに到達する interface の task・関数・clocking ブロック・
  入れ子インスタンス。virtual interface 越しのネットへの書き込み。
- 既定の寿命（`interface automatic`）が、使用先モジュールと異なる interface。

### program {#programs}

`program`（Clause 24）の `initial` 手続きは reactive region セットで実行します。
モジュール内に入れ子にした program、匿名 program、インスタンス化されていない
program の暗黙トップ扱い、program 内の clocking ブロックは未対応です。
region に関する逸脱2件は[意図的な逸脱](#intentional-deviations)に挙げています。

### 型と式

- tagged union、unpacked union、packed union の代入パターン。
- `chandle` の固定長 unpacked 配列（`chandle a[4]`、typedef 経由を含む）。
- enum の packed 配列の要素に対する enum メソッド（`first`、`next` など）
  （[#2417](https://github.com/celox-sim/celox/issues/2417)）。
- ベクタの範囲外に出る定数部分選択の読み取り、配列要素または内側 packed
  次元に対する宣言範囲を出る定数部分選択の書き込み
  （[#88](https://github.com/celox-sim/celox/issues/88)）。
  素の packed ベクタへの一部が範囲外になる定数部分選択の書き込み
  （例: `logic [7:0] q` への `q[9:6] = v`）は対応し、範囲内のビット
  （`q[7:6]`）だけへ書き込みます。実行時選択は標準に従います。
- パラメータ式中の縮約演算子（[#456](https://github.com/celox-sim/celox/issues/456)）。
- 型演算子（6.23）: 型参照と値の比較、型に対する `case` の範囲項目、
  ポートを読む式や generate ブロック内の式への適用。
- 型名 <a id="type-names"></a>: `$typename`（20.6.1）は一意な綴りを持たない型に対して
  処理系生成名を作らず、拒否します。匿名の enum・構造体・union、soft および
  tagged union、unpacked 配列メンバを持つ unpacked 構造体、enum や符号付き
  typedef の packed 配列、整数でない値パラメータを持つクラス、
  `$typename(a + b)` のような名前やデータ型でない被演算子が該当します。
- unpacked 配列型のパラメータは開始時から値を持ちますが、展開時に必要な定数としては
  使えません。packed 構造体パラメータのメンバは展開時定数ですが、127 ビットを超える
  メンバと X・Z ビットを持つメンバは、宣言の寸法、generate 構文の選択、パラメータの
  上書きには使えません。

### unpacked 構造体 {#unpacked-structures}

- ポート、ネット、パラメータとしての unpacked 構造体。`static`、`rand`、
  `const` のクラスプロパティとしての unpacked 構造体。
- enum、packed 集約、`string`、ハンドル、unpacked 構造体型の配列メンバ。
  可変長次元や宣言初期値を持つ配列メンバ。配列メンバを持つ構造体はポート、
  クラスプロパティ、サブルーチン引数、動的コンテナ要素にできません。
- `string` やハンドルのメンバ、2値と4値の混在、初期値宣言を持つ構造体の
  固定長配列。そのような配列のポート・クラスプロパティ・サブルーチン引数化。
- enum や非整数メンバの初期値、入れ子構造体全体のメンバ初期化子、
  既定値付き構造体型のポート・サブルーチン引数・動的コンテナ要素化。
- 上記以外の `real` メンバや配列メンバ。動的配列・キュー・連想配列のメンバ化を含みます。

### プロセス・タイミング・制御 {#timing-controls}

- 手続き文としての `force` / `release` と、`disable fork` 以外の `disable` 文。
- `initial` / `always` プロシージャ以外に書かれた手続き的継続代入
  （`assign` / `deassign`、IEEE 1800-2023 10.6.1）。サブルーチン、`final`
  プロシージャ、`always_comb` / `always_latch` / `always_ff` 内は非対応です。
  これを含む `always` プロシージャはプロセスとして実行されます。対象は
  モジュールの packed な整数型変数全体に限られ、連接、`real`・`string`・
  ハンドル・unpacked 変数、手続きブロック内の変数は拒否されます。副作用の
  ある式、サブルーチンやシステム関数を呼ぶ式、手続きブロック内の変数を読む式、
  `always @*` プロシージャも代入する対象も拒否されます。
- `assign` が有効な間、単純な代入文以外による対象変数への書き込み（出力引数、
  連接の代入先、遅延付きノンブロッキング代入、他モジュールからの階層名）は
  抑止されず、同じタイムステップ内で上書きされます。その変数を待つプロセスは
  書き込まれた値を観測できます。
- `always_comb` 内のノンブロッキング代入
  （[#453](https://github.com/celox-sim/celox/issues/453)）と
  `always_latch` 内のノンブロッキング代入
  （[#431](https://github.com/celox-sim/celox/issues/431)）。ブロッキング代入を使ってください。
- `always_ff` のイベントリストの `iff`
  （[#452](https://github.com/celox-sim/celox/issues/452)）、素の信号以外の
  `always_ff` エッジ被演算子（[#464](https://github.com/celox-sim/celox/issues/464)）、
  `always_ff` の複数ビットエッジ信号、非同期リセットの無い複数エッジ。
  素の `always` の同じ形は時間を持つプロセスとして実行します。[実行モデル](#celox-execution-model)を参照してください。
- 代入内タイミング: ノンブロッキング代入内イベント制御と、定数でない遅延。
  遅延ノンブロッキング代入（`lhs <= #constant rhs`、10.4.2）は整数定数遅延と
  時間リテラルのみ、`initial` と通常の `always` でのみ使えます。
- 遅延付き継続代入 <a id="continuous-assignment-delays"></a>: 非負整数定数または時間リテラル1つを超えるもの
  （[#444](https://github.com/celox-sim/celox/issues/444)）。rise/fall/turn-off 遅延リスト、
  変数・負数・単位なし小数 real の量、連結・集約先、動的選択先、
  スカラまたは packed 整数でない先、サブルーチン呼び出しを含む右辺は未対応です。
  独立ネット遅延（`wire #5 w;`）では、ポート接続内の遅延ネット、遅延付き入力・
  `inout` ポート、unpacked・非整数の遅延ネット、動的選択先を持つドライバが未対応です。
- プロセス内のタイミング制御: イベント式中の不純な関数呼び出し。static な task 引数を
  読む `wait`・イベント式。クラスプロパティを読むイベント式（直接・ハンドル・関数経由・
  プロパティ内コンテナを含む）。`rand_mode()` や `constraint_mode()` を読む
  `wait` 条件。task や副作用のある関数を呼ぶ、または時刻・乱数・mailbox・process を
  読む、クラスプロパティ上の `wait` 条件。
- static 引数を持つ package および `$unit` の時間付き task。
- 宣言初期化子、`final`、`always_comb`、`always_ff` 内の `fork`
  （これらの文脈にはスケジューリング呼び出し用のプロセス基盤がありません）。
  fork 共有の自動ローカル上の複合イベント式では、グローバル/static の混在依存、
  外部呼び出し、スクラッチ状態を保持する関数、関数・シード/plusargs 結果・
  `$readmemh` などのシステムタスクから監視対象への暗黙書き込みを拒否します。
- named event <a id="named-events"></a>: event の配列、event ポート、unpacked 構造体の
  event メンバ、event を返す関数、`always_comb` / `always_latch` 内のトリガ、
  遅延やイベント制御付き `->>`、信号変化やエッジと混ぜたイベント制御
  （`@(e or posedge clk)`）、他被演算子と混ぜた `triggered` の `wait` 条件や
  条件が呼ぶ関数からの読み、`wait_order` は未対応です。
- 型パラメータ無し mailbox。64ビットまでの整数型、`string`、クラス・
  `mailbox`・`semaphore`・`process`・`event` ハンドル以外の mailbox 要素型。
- 組み込み `process` クラスの構築・継承。関数からの自スレッド停止や `await()`。
- ループ: 手続きループの展開は最大10,000回。実行時に回数が決まる組み合わせループは
  数え上げ `for` のみ。10,000回を超える loop-generate は拒否します
  （[#448](https://github.com/celox-sim/celox/issues/448)）。
  genvar のビット複合代入での更新も拒否します
  （[#455](https://github.com/celox-sim/celox/issues/455)）。
  generate 中の再帰的な定数関数展開も拒否します。
- `final` 手続き <a id="final-procedures"></a>: タイミング制御とユーザー task 呼び出し
  （関数経由を含む）。戻った後にイベントをスケジュールする関数（13.4.4）。
  `final` 内で宣言した `fork … join_none` の子は初期化されますが開始しません。
  [実行モデル](#celox-execution-model)も参照してください。

### クラス {#classes}

- クラスハンドルでない変数への `$cast`。
- クラスパラメータ値としての匿名 enum・構造体・union 型、およびモジュールや
  package の名前付き4値・string 定数（クラス自身の同種パラメータは名前で渡せます）。
- package クラス特殊化の型引数としてのモジュール内クラス。型引数としての
  匿名 enum・構造体・union 型。
- `real` または固定長配列のプロパティ。プロパティの packed 構造体メンバ。
  unpacked 構造体型プロパティのメンバ選択。`[N-1:0]` で宣言していない
  プロパティのビット・部分選択の読み。プロパティへのノンブロッキング代入。
  `output` / `inout` 引数としてのプロパティ。`const` な動的コンテナプロパティ。
- インライン `randomize() with` 制約で無修飾の static プロパティ名は、
  同名の呼び出し側変数へフォールバックせず拒否します。

### ランダム化と制約

- スコープ `randomize()` / `std::randomize()`（18.12）: `with` 制約、未対応・
  未解決ドメインの enum、unpacked 配列、ネット、定数、選択、連結引数は未対応です。
- クラス `randomize()`: 4値・集約プロパティ、static な rand プロパティ、配列・
  オブジェクト・string の rand プロパティ、制約付き enum と `randc`、`foreach`・
  `soft` 制約、`dist`、`solve before`、外部・static 制約ブロック、制約中の
  型パラメータ、`inside` 集合の配列集合・X/Z ワイルドカード・`$` 境界、
  1–8ビット整数と256メンバまでのスカラ enum を超える `randc` ドメインは未対応です。
  名前付き制約ブロックは、比較・論理・ビット演算・加減乗算を使う
  1–64ビット2値スカラ整数プロパティだけを扱います。
- インライン `with` 制約: 4値・集約の呼び出し側入力、型パラメータ入力、
  修飾付き `this` / `super` 参照、制限付き `with (names)` 検索、
  スコープ `std::randomize(…) with` は未対応です。
- 未解決・X/Z 値を持つ enum のランダム化。

### 動的コンテナ・文字列・参照引数

- コンテナ: 一部操作での多次元動的コンテナ、`real` 要素、対応キー型を超える
  ワイルドカード・クラスキー、スライス、キー付きパターン、連結内の連想配列、
  ノンブロッキング要素代入、コンテナや添字に副作用のある、または要素が
  `string`・ハンドル・コンテナの複合要素代入、コンテナポート、`const` コンテナ。
  `new[]` 初期化子は互換な動的配列・キュー、または代入パターンで、`default:` だけの
  パターンは新しい全要素にその値を与えます。固定長配列、他のキーと併用した `default:`、
  要素がコンテナや unpacked 構造体の場合の `default:` は未対応です。
  連想配列リテラルの明示添字項目、および添字項目と `default:` の混在は未対応です
  （`default:` のみのリテラルは対応、7.8.6）。
- コンテナメソッド: 64ビットを超える整数キーの添字を返すロケータメソッド。
  64ビット超要素のソートや `with` 付きソート。固定配列の並べ替え。
  64ビット超要素コンテナの `==` 比較・縮約・`%p` 出力。string・クラス値の
  キュー pop 結果への型照会（`$bits` など）。要素次元と連想境界の照会。
  継続代入内・多次元固定配列上・破棄文としてのロケータメソッド。
  ロケータ部分集合以外の縮約・ロケータ `with` 式とイテレータ引数。
- コンテナ仮引数: コンテナ型の関数結果、`output` / `inout` 実引数としての
  クラスプロパティやコンテナ要素、要素型が仮引数と異なる実引数。
- 文字列 <a id="strings"></a>: `atoreal` / `realtoa`。`$value$plusargs` の書式としての
  string 変数、ファイル I/O の string 引数、VPI・VCD 出力・TypeScript API での
  string 値。
  固定長文字列配列へのノンブロッキング文字書き込み。文字列配列全体や
  部分配列へのメソッド呼び出し。配列値の string 引数・戻り値。
- 参照引数 <a id="reference-arguments"></a>（13.5.2）: `ref static`。動的配列・キュー・
  連想配列の要素の実引数化。クラスプロパティの要素。クラスプロパティやその
  メンバに束縛した仮引数の一部への書き込み。実行時にしか位置が定まらない、
  配列要素に束縛した仮引数の選択。
- 名前付き引数: 選択添字に手続き的副作用のある名前付き呼び出し。受け手や
  オブジェクトを読む先行引数に副作用がある場合の、`always_comb` や `assign`
  内で受け手を使うメソッド既定値。

### アサーション

- 遅延即時アサーション（`assert #0`、`assert final` など）: アクションは
  表示タスクまたは重大度タスクの呼び出し1つだけです。ユーザーサブルーチンや
  複数文は拒否します。
- 引数無しの `$asserton` / `$assertoff`（デザイン中の全アサーションの検査を停止・再開）
  以外のアサーション制御タスク（IEEE 1800-2023 20.11）: `$assertkill`、
  `$assertcontrol`、アクション制御タスク（`$assertpasson` など）、およびレベルや
  スコープ・アサーションのリストを引数に取る形。`$asserton` / `$assertoff` は
  パッケージ内とコンパイル単位スコープでは拒否されます。パッケージのサブルーチンと
  そこで宣言されたクラス内のアサーションは制御されません。これらを呼ぶ `always_comb` /
  `always_latch` は、`initial` やエッジ制御のもの以外の手続きもこれらを呼ぶ場合に拒否されます。
  これらを呼ぶデザインでは、一致の仕方が複数ある並行シーケンス（サイクル遅延範囲や `or`）を拒否します。
- サンプル値 <a id="sampled-values"></a>: 明示クロック引数付き、推論するクロック無し、
  入れ子（`$past($past(a))`）、unpacked 配列を読む被演算子の
  `$past`・`$rose`・`$fell`・`$stable`・`$changed` は未対応です。

### 並行アサーション {#concurrent-assertions}

対応するのは次の部分集合だけです。Clause 16 の並行アサーションの残りは未対応です。

- 明示クロック付きの Boolean な `assert property`・`assume property`・
  `cover property`（モジュール・generate 項目）。固定クロック数だけ離れた
  ステップ列（`a ##1 b`、最大1024ティック）を `|->` または `|=>` で連結したもの。
- 上限のあるサイクル遅延範囲（`a ##[1:3] b`、先頭にも書けます）と、被演算子に match 項目を持たない
  sequence の `or`・`and`・`intersect`、`b throughout s`。
  試行は一致の仕方のいずれかが一致した最初のティックで成功し、どれも一致し得なく
  なった最初のティックで失敗します。一致の仕方は最大1024通りで、property 全体が
  その sequence である場合だけです（含意、`expect` 文、ローカル変数や match 項目を
  持つものは未対応）。
- 複数サイクル sequence への `disable iff`。条件が試行の開始から終了までのいずれかの時点で
  真になると（クロックのティックの間も含みます）、その試行は無効となり成功も失敗もしません。
  条件はサンプル値ではなく現在の値で評価します（16.12）。
- property 全体である複数クロックの sequence（16.13.1）。一致の仕方が 1 通りの単一クロックの
  部分 sequence を、クロックの切り替わりで `##1` または `##0` により連結したもの
  （`@(posedge c0) a ##1 @(posedge c1) b`、各クロックはエッジのリスト）。`##1` では次の部分
  sequence が前の部分 sequence の終了より厳密に後の最も近いクロックのティックで、`##0` では
  終了と同じティックも含めて始まります。`disable iff`、含意、ローカル変数や match 項目とは
  併用できず、クロックの切り替わりをまたぐその他の演算子は拒否します。
- 2 つの Boolean 式の `p iff q`。`|->` の後件にも書けます（`a |-> b iff c` は `a |-> (b iff c)`）。
- プロセスや task 内の明示クロック付き `expect (property_spec) action_block`（16.17）。
  次のクロックイベントから 1 回だけ評価し、各ステップをサンプル値で判定した後、Reactive
  領域で成功・失敗の文（既定は `$error`）を実行して続行します。property は Boolean、
  含意、固定遅延の sequence（名前付きも可）で、`disable iff`・ローカル変数・match 項目は未対応です。
- 仮引数無しの名前付き property・sequence（同一モジュール、generate 外、
  本体が同部分集合内）。組み込み整数型のローカル変数と match 項目。ローカル変数を持たない
  名前付き sequence は、同じクロックか先頭にあれば別の sequence の被演算子にできます。
- 時間無しのブロッキングアクション、報告・重大度タスク、停止・ノンブロッキング
  代入・プロセス開始や制御・イベント発行をしないサブルーチン呼び出し。

特に未対応: 手続き的並行アサーション、暗黙・既定クロック、仮引数付きや
他スコープの名前付き property・sequence、上限の無いサイクル遅延範囲（`##[1:$]`）、
繰り返し、sequence 演算子 `within`、同じティックで一致し得ない sequence の `intersect`、
異なるクロックやローカル変数を持つ名前付き sequence の sequence 内インスタンス、複数サイクル
sequence 内のインスタンス、
複数サイクルや名前付きの被演算子を持つ `iff`、
述語や match 項目値中のユーザー関数呼び出し、時間付き・ノンブロッキングなアクション文。

### システムタスク・関数と I/O

- Celox は IEEE 1800-2023 が定義する全システムタスク・関数名（Annex D を含む）を
  知識として持ちます。呼び出し位置で未対応のもの（例: `$get_coverage`）は、
  未使用パラメータや関数本体など、書かれた場所によらず名前付きで報告します。
- `string` 値への `%t`。選択したインスタンス配列要素や
  generate パスへの時刻照会（`$timeunit`、`$timeprecision`、`$printtimescale`）。
  明示・継承した時間尺度と処理系既定値の混在（3.14.2.3）。
- 組み合わせプロセスや継続代入からの実行時 I/O 呼び出し
  （`$fopen`、ファイル向け `$display`、`$system`、plusargs など）と
  `$sformatf` 系呼び出し。WebAssembly ホストでの `$system`。
- ファイル I/O の部分集合: `$strobe` / `$fstrobe` と `$monitor` / `$fmonitor` は
  スケジュール済みプロセス内のモジュール/static 式のみ受理します（サブルーチン内、
  手続きローカル、展開済みループ変数、更新、副作用のある引数は拒否）。
  `$monitor` が観測するのは static スカラ変数全体、コンソール `string` 変数、
  定数、直接の時刻照会だけです。packed バッファ・クラスプロパティ・動的コンテナへの
  `$fgets`、クラスプロパティ・動的コンテナへの `$ferror`、`$sscanf` / `$fscanf` の
  `%s` 出力と `%t`、`byte` の unpacked 配列への `$sformat` / `$swrite`、
  動的 `string` および非 UTF-8 のファイル名・モード、動的・連想配列の
  `$writememh` / `$writememb` は未対応です。
- 書式付き文字列出力 <a id="formatted-output-to-strings"></a>: 関数呼び出しやキュー pop から得た
  enum 値、packed 共用体、関数呼び出しの戻り値やその他の副作用のある呼び出しで得た packed 構造体、enum の packed 配列、固定長 unpacked 配列、
  `real` メンバを持つ構造体、その他の集約関数結果、他の集約のコンテナに対する
  `%p` は未対応です。実行時書式の `%p` は string を引用符付きで、それ以外を
  `%0d` として出力します。
- plusargs: `$value$plusargs` の `real` 変換と、書式としての string 変数。
  `string` の出力先は `%s` 変換のみ受け付けます。

## 意図的な逸脱 {#intentional-deviations}

受理しますが標準どおりではありません。移植性が必要なコードはこれらに依存しないでください。

- **推論ラッチは既定でエラーです。** `always_comb` の不完全な代入
  （一部の経路でしか書かない static ローカルを含む）は、
  [`[diagnostics]` テーブル](./celox-toml.md#diagnostics)で
  `inferred_latch = "warn"` または `"allow"` を設定するか、相当する
  Rust / JavaScript オプションを使わない限り拒否します。明示的な
  `always_latch` と通常の `always @*` には不要です。
- **`static` キーワードの欠落を1つのケースで許容します。** IEEE 1800-2023 6.21 は、
  初期化値を持つ static 変数の宣言に明示的な `static` を要求します。Celox は
  定数初期化子ではどの手続きでも、実行時初期化子では `initial` のような
  1度だけ実行する手続きで、キーワード無しの宣言も受け付けます。`always` 内の
  実行時初期化子にキーワードが無い場合は拒否します。`static` または
  `automatic` を明示してください。
- **定数ファイル名の `$readmemh` / `$readmemb` はコンパイル時に読みます。** ファイルは
  呼び出し時ではなくコンパイル時に読み、欠落・不正形式はコンパイル時の `MemoryFile`
  エラーです。`always_ff` では活性化のたびにファイルのワードを書き込みます。要求範囲外の
  `@` アドレス指令はエラーです。指令無しでワード数が合わない場合は
  コンパイル警告となり、余分なワードは無視し、不足分はメモリを変更しません。
  定数文字列でないファイル名（`$value$plusargs` が読む `string` 変数など）は、
  プロセス内で呼び出しが実行された時に読みます。同じエラーはシミュレーションを終了させ、
  ワード数の不一致は実行時警告になります。この形のメモリは unpacked 配列全体に限ります。
- **program の継続代入は Reactive region で実行しません。** program 内の
  継続代入はデザインの組み合わせ論理と評価し、program が書くデザイン論理は
  reactive region セットの実行中に安定します。そのため `#0` の後に再開した
  program は新しい値を読みます。IEEE 1800-2023 4.5 では reactive region セットが
  空になるまで評価しません。Icarus、Verilator、xezim も Celox と同じです。
- **program 外の `$exit` は警告します。** program スレッド以外からの `$exit` は
  24.7 どおり無視しますが、Celox はコンパイル警告（`Simulator::warnings()`）も
  出します。未使用関数の宣言内呼び出しも含みます。
- **カバレッジは報告しません。** 即時 `cover` は pass 文を実行しますが、
  カバレッジカウンタやレポートは公開しません。
- **オプトインの旧式空縮約。** 既定では空配列の縮約は恒等元を返します（7.12.3）。
  Rust の `SimulatorBuilder::legacy_empty_reductions(true)`
  （フロントエンドの `AnalysisOptions::legacy_empty_reductions`）では全ての
  空縮約がゼロを返します。そのようなツールとの互換用です。
- **UVM の DPI 関数は組み込みです。** UVM の `src/dpi` の C 名での import は
  ユーザー C 関数ではなく Celox の実装に束縛します。[DPI-C imports](#dpi-c-imports)を参照してください。

## 処理系定義・順序未規定の挙動 {#semantics-worth-knowing}

IEEE 1800-2023 が処理系に委ねる点、順序を指定しない点、非決定的とする点について、
Celox は次のように選びます。移植可能なコードはこれらに依存しないでください。

### イベント実行順序 {#timing-semantics}

Clause 4.7 がどちらの順序も許すケースで、Celox は決定的です。

- 継続代入・組み合わせプロセス・イベント式が時間を持つプロセスの書き込みを見るのは、
  そのプロセスがタイミング制御に達したとき、または終わったときです。
  `b = 1; a = 1;` の後では `a ^ b` は変化しておらず、パルス `a = 1; a = 0;` は
  `@(a)` を起こしますが `@(a ^ b)` は起こしません。式待ちに見せたい場合は
  書き込みの間に `#0` を置いてください。fork 共有自動ローカル上の複合待ちは、
  対応するブロッキング代入と文呼び出しのコピーアウトを個別に観測します。
- 待ち側は、`always_comb`・`always @*`・エッジ駆動プロセスの1回の評価が書く
  全ての値を見ます。後の書き込みで上書きされる値も含みます（4.3、9.4.2）。
  子インスタンスの出力ポート（継続代入、23.3.3）越しでは、親の待ち側は
  安定した値だけを見ます。
- `expr iff condition` 項目（9.4.2.3）は、両者が変数の場合 `expr` の各書き込みで
  判定します。1プロセス内では `clk = 1; en = 0;` はイベントとなり、
  `en = 0; clk = 1;` はなりません。どちらかが一般式やネットの場合、または
  `expr` をフリップフロップ領域が書く場合
  （[#1830](https://github.com/celox-sim/celox/issues/1830)）は、発生検出時
  （組み合わせ論理の安定後）に判定します。
- クラスプロパティを読む `wait` 条件は、それが読むプロパティの全てのストアの後に
  再確認します。どのハンドル越しでも、各 `randomize()` の後でもです。別オブジェクトの
  同プロパティや、すでに保持している値のストアでも再確認するため、最小限より
  頻繁に確認することがあります（9.4.3）。
- 非同期リセットがある場合、`$past` の履歴ティックはクロックがタイムスロット前の
  レベルを離れた実行です。スロット内で同レベルに戻るクロックは履歴を進めません。
  1タイムステップ内の複数クロック遷移は並行アサーションでは未定義です（16.5）。

### 時刻と精度 {#time-and-precision}

- 処理系既定の時間尺度は 1 ns / 1 ps です。`Simulation` はデザイン全体の
  時間精度（3.14.3）で数えます。既定では 1 ps なので、時間尺度の無い
  デザインでは `%t` はピコ秒を報告します。
- `$timeformat`（20.4.3）は、以降の `%t` 変換の単位・小数桁数・接尾辞・
  最小フィールド幅を、すべての出力と `$sformatf` について設定します。時刻は
  0 から遠ざかる方向に四捨五入し、`%0t` のように変換に幅があればそれが
  最小フィールド幅に代わります。接尾辞は NUL 文字も含めた文字列リテラル
  です。これを（サブルーチン経由でも）呼ぶ `always_comb`・`always_latch`・
  `always @*` はプロセスとして実行し、時刻 0 に一度（`always @*` を除く）、
  その後は読む値が変わるたびに実行します。実行時に計算した単位数が
  2〜-15 の外、または精度・フィールド幅が負のときは、定数なら解析エラーに
  なるのと同様にシミュレーションがエラーで終わります。Celox が出力する
  小数桁数と埋める幅はそれぞれ最大 4096 で、それを超える値はエラーです。
- `final` 手続きは決定的ですが順序未規定の順に逐次実行します。コンストラクタや
  関数を呼ぶ package・モジュール変数初期化子の相対順序も未規定です（6.8、10.5）。
  依存しないでください。

### 乱数 {#random-numbers}

- `$urandom` は決定的な32ビット線形合同生成器を使います（18.13.1、18.14）。
  既定シードと系列は処理系定義で、他シミュレータと一致する必要はありません。
  `$random` と `$dist_*` は Annex N に従うため、同附属書の他実装と一致します。
  `process` とクラスオブジェクトの乱数状態文字列（`get_randstate`、18.13.4）は
  不透明で Celox 固有です。`set_randstate`（18.13.5）は `get_randstate` が返した
  文字列だけを受け付け、それ以外ではシミュレーションがエラーで終わります。
- 組み合わせ手続きでは、プロセス活性化時に乱数を引きます。入力不変の
  再評価ではストリームは進みません。
- 制約付き `randomize()` はオブジェクトのシードで充足代入を探索し、選択した
  全プロパティをまとめて更新します。一様分布は保証しません
  （[#2187](https://github.com/celox-sim/celox/issues/2187)）。
  不充足が証明された場合は `0` を返し、プロパティを保持し、post フックを
  省略します（18.6.3）。資源枯渇は実行時エラーです。
- 配列の `shuffle` は呼び出しプロセスの乱数ストリームから引きます。

### ホスト依存の結果

- `$ferror` のコードと文言はホスト依存です。Celox は失敗した `$fopen` の
  詳細を FD 0 でも公開します。
- `$system` は C の `system()` の値を返します。ホストの終了状態エンコーディングを
  含みます（20.17.1）。相対ファイルパスはホストプロセスの作業ディレクトリ基準です。
- 読み込みバイトが無い `$fgets` は送信先を変更しません。21.3.4.2 は返却する
  件数を規定しますが、その場合の送信先の値は規定しません。

## Celox の実行モデル {#celox-execution-model}

以下は SystemVerilog ではなく Celox API の選択です。どのデザインに時間付き
`Simulation` API が必要か、ホストが何を観測できるかを決めます。

- **`Simulator` はプロセスを実行しません。** 定数初期化は両 API に適用されます。
  `final` 手続き、デザイン状態を読む・システムタスクを呼ぶ `initial` ブロック、
  タイミング制御を持つプロセス、`@*` でも文が読む変数そのものでもない
  センシティビティリストを持つ `always` はプロセスとして実行するため
  `Simulation` が必要です。`Simulator` で構築すると
  `SimulatorErrorKind::ProcessRequiresSimulation` で失敗し、最初のプロセスと
  ソース位置を示します。リスト（部分選択を含む）が文の読み対象をちょうど覆い、
  タイミング制御・ノンブロッキング代入・サブルーチンやシステムタスク呼び出し・
  自己読み・重複ストアを持たない `always` は、両 API で `always @*` のように
  振る舞います。フリップフロップ形のエッジ駆動 `always` も同様です。
  `Simulator::tick` は自イベントの立ち上がりだけを駆動します。`Simulation` は
  全エッジを実行します。`Simulator` の1呼び出しは `$sampled` と遅延アサーションに
  ついて1タイムスロットであり、`$time` は 0 を返します。
- **終了。** `final` 手続きは `$finish`・`$fatal`・明示的な
  `Simulation::finish()` で1度実行します。空の `step()` や `run_until()` の
  終端では、再利用可能な Rust シミュレーションは終了しません。その後は
  スケジュール済みイベントは実行しないため、`final` 内で直接行った
  ノンブロッキング代入のキュー更新は適用されません。`initial` 内の `$stop` は
  シミュレーションを停止します（`is_stopped()` が立ち、次回呼び出しで継続）。
  `always` 内では `RuntimeEvent::Stop` として、`$finish` は
  `RuntimeEvent::Finish` として報告します。VPI ホストなど対話モードの無い
  バッチホストは `$stop` で実行を終えます。停止中はチェックポイントを取れません
  （`CheckpointError::Stopped`）。
- **報告。** `$info`・`$warning`・`$error`・`$fatal`（失敗アクション無し
  アサーションの既定 `$error` を含む）は、重大度・メッセージ・ファイル・行・
  スコープ・シミュレーション時刻を持つ `RuntimeEvent::Report` として
  ホストへ届きます。`celox` 実行を失敗とするのは `$error` と `$fatal` だけで、
  `$fatal` はシミュレーションも終えます。
- **観測できる名前。** interface は使用モジュールへ展開します。interface
  インスタンス `h` のメンバ `m` は信号 `h$m` となり、トップの interface ポートは
  `bus$data` のようなポートとなり、汎用 `interface` ポートを持つモジュールは
  接続 interface ごとに `M$I` として複製します。インスタンス配列要素は宣言添字で
  指定します（`dut.u[3]`、`child_signal(&[("u", 3)], "y")`）。package 変数は
  `p::v`、コンパイル単位変数は `$unit::v` と `sim.signal(…)` で名指します。
  階層名はホスト API 経由で `string` 変数にも到達できます。
- **ヒープとチェックポイント。** 文字列・クラスオブジェクト・コンテナは、
  タイムスロット間と各 `tick` 後に回収するヒープにあります。`string`・
  クラスハンドル・`process` ハンドル・virtual interface は32ビット整数のように
  保持します。コアチェックポイント（`checkpoint()` / `restore()`）は
  ヒープオブジェクト、fork の子、process ハンドル、乱数ストリーム、保留中
  strobe/monitor 状態、遅延アサーションキューを保持します。移植用状態ファイルは
  狭く、変数がクラスオブジェクト・semaphore・mailbox・公開済み `process`
  ハンドルを持つ間、プロセスが named event を待つ間や現在のトリガが
  `triggered` に見える間、strobe 出力が保留中の間、ファイル I/O・ソース VCD 制御・
  monitor タスクを持つデザインでは保存に失敗します。非 null のクラスオブジェクトや
  動的コンテナ、保留 NBA・ホスト書き込み・イベント待ち基準内の空でない
  string/オブジェクト値もコアチェックポイントを必要とします。旧ヒープ・
  コンテナレイアウトのネイティブイメージと直列化チェックポイントは再生成が必要です。
- **ループの進行。** 進行しなくなった `always_ff` 内実行時ループは、
  ループ変数名を含む実行時エラーを報告します。

## Veryl と SystemVerilog の混在 {#mixed-language}

言語混在は標準ではなく Celox の機能です。
`Simulator::from_mixed_sources`
（または `Simulation::from_sources(veryl).with_sv_sources(sv)`）が両ソースを
組み合わせます。Veryl は `$sv::Module` で SV モジュールをインスタンス化します。
SV は Veryl モジュールや interface を出力名で参照します
（`prj_Module`、`omit_project_prefix` 時は `Module`、`Veryl.toml` 無しでは
プロジェクト名は `prj`）。どちらの言語もトップにできます。
interface と `input` / `output` modport は双方向に境界を跨げます。

境界越しに未対応: 言語を跨ぐ interface 関数呼び出しと関数の `import` /
`export` modport。契約で表せない文本体を持つ interface ヘッダ関数（ループなど）、
ヘッダ依存の仮引数・戻り値幅、対象サイズが interface パラメータに依存する
宣言キャスト、記号的に下げられない定数関数。これらは単一言語内では使えます。

## DPI-C imports {#dpi-c-imports}

`import "DPI-C"` で宣言した C 関数を呼べます。`celox run` は
`--dpi-lib PATH`（繰り返し可）と `celox.toml` の `dpi_libs` で指定した
共有ライブラリをリンクします。Rust API では、DPI-C 登録はビルダーのホスト API です。

```rust
extern "C" fn c_add(x: i32, y: i32) -> i32 { x.wrapping_add(y) }

// SAFETY: `c_add` and the functions of libmodel.so have the signatures
// their imports declare.
let mut sim = unsafe {
    Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .dpi_function("c_add", c_add as *const ())
        .dpi_library("libmodel.so")
}
.build()?;
```

各 import は構築時に C 名でリンクします。`dpi_function` 登録を最初に探し、
次に `dpi_library` のライブラリを順に探します。解決できない名前は `Dpi` エラーで
構築に失敗します。C 関数と import の一致を Celox は検証できないため、両方とも
`unsafe` です。

対応部分集合（他は全て拒否）:

- モジュール、package、またはコンパイル単位スコープで宣言した `function` import
  （task でも `context` でもない）。手続きコードからの呼び出し: `initial`、
  `always`（`always @*` を含む）、`always_ff`、およびそれらが呼ぶサブルーチン。
  継続代入と `always_comb` からの呼び出しは拒否します。
- 引数は最大16個。`input`・`output`・`inout`。
- 引数型は `bit`、`logic`（`svBit` / `svLogic` として）、`byte`、`shortint`、
  `int`、`longint`（符号有無可）、`chandle`、`string`（`const char *` として）、
  `bit`・`logic`・`reg` の packed 配列、`integer`、`time`
  （`svBitVecVal` / `svLogicVecVal` のワード列として）。結果型は packed 配列より前の型と
  `void`（35.5.5）。`real`、`shortreal`、名前付きの型（typedef、列挙型、構造体）、
  unpacked 配列と open array は拒否します。
- `output`・`inout` 引数はその C 型へのポインタ、`string` では `const char *` への
  ポインタです。関数に渡した string と packed 配列は関数が戻るまで有効です。
  関数が返す、または output に書く string は、関数が戻った時点でコピーします。
- 2値シミュレーションでは、関数が `logic` 引数に書いた不定ビットは 0 として読みます。
- `export "DPI-C"` 宣言は受理して検査しますが（35.7）、呼べるのは
  `context` import だけ（35.5.3）なので、C コードからは呼べません。
- UVM の C ライブラリ関数は Celox が提供し、同名の
  ユーザー実装を覆い隠します。`uvm_re_*` は Rust の `regex` クレートを使うため、
  まれな構文で POSIX 拡張正規表現と異なります。`uvm_dpi_get_next_arg_c` は
  `celox` と plusargs を返し、`uvm_dpi_get_tool_name_c` は `Celox` を返します。
  `uvm_hdl_*` と `uvm_polling_*` は 0 または `null` を返すため、UVM は
  それらの機能を利用不可と報告します。

## Veryl が出力する SystemVerilog {#systemverilog-emitted-by-veryl}

Veryl の適合性スイートは Veryl が出力した SystemVerilog に対しても実行します。
残る対象外は次のとおりです。

- 位置項目と `default:` を混ぜた配列リテラル（`'{a, default: b}`）に対して
  Veryl が不正な SystemVerilog を出力します。
- `always_ff` 内で出力引数や副作用のある Veryl 関数呼び出しをオプトインした
  デザインには SystemVerilog での等価物が無く、出力しません。
- テストベンチモジュール（クロックと `$finish` スケジューリングを持つ
  `initial` ブロック、階層代入）はシミュレート可能な SystemVerilog としては
  出力しません。


