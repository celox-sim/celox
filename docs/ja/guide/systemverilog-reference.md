---
outline: [2, 3]
---

# SystemVerilog の対応範囲と逸脱事項

導入と最初のシミュレーションは [SystemVerilog サポート](./systemverilog)から始めてください。

## 前提: IEEE 1800-2023 {#what-is-supported}

Celox は IEEE Std 1800™-2023 が規定する SystemVerilog を実装します。
このページは標準をなぞらず、標準との差分だけを次の5つに分類してまとめます。

1. **未対応の機能** — 未実装でコンパイル時に拒否される構文や節。
2. **部分対応の機能** — 特定の部分集合を実装している機能。各項目で**対応**と**未対応**を並べて示します。
3. **個別制約** — 全体としては標準に従う機能における、個別のエッジケース制約。
4. **意図的な逸脱** — [標準準拠コードに影響する逸脱](#deviations-standard-code)と[標準外コードの許容](#deviations-nonstandard-code)に分けて記載します。
5. **処理系定義の選択と Celox の実行モデル** — 標準が処理系に委ねる選択、および Rust の `Simulator` / `Simulation` の分離・ホストクロック・複数トップモジュール・チェックポイント・メモリファイル・言語混在・DPI-C 登録など Celox 固有の API。

### 適合保証の範囲: 標準準拠コードと標準外コード

- **標準準拠コード:** このページに記載のない未対応項目や制限・逸脱に該当しない限り、有効な IEEE 1800-2023 コードは標準どおりに実行されます（未実装機能は黙って誤動作させず、構文名と追跡 issue 番号 — 個別 issue が無い場合はフロントエンドのロードマップ [#88](https://github.com/celox-sim/celox/issues/88) — を含む `Unsupported` エラーを返します）。記載のない差異が見つかった場合は不具合として [issue を登録](https://github.com/celox-sim/celox/issues)してください。
- **標準外コード（既定モードと strict モード）:** 標準外コードの受理は標準コードの誤動作とは重要度が異なるため、**既定モード**では実用上の互換性緩和を警告付きでいくつか受理します（[標準外コードの許容](#deviations-nonstandard-code)・[`[diagnostics]`](./celox-toml.md#diagnostics)参照）。**strict モード**（`strict = true` / `--strict` / `.strict(true)`）ではこれらの互換診断がエラーになり、**下記に明記した意図的な拡張を除いて strict モードで IEEE 1800-2023 外のコードを受け入れてしまう場合は不具合です**。[issue を登録](https://github.com/celox-sim/celox/issues)してください。

測定した適合状況は[検証結果のページ](/ja/verification/)で公開しています。

## 未対応の機能 {#what-is-not-supported}

以下の構文や節は未実装であり、コンパイル時に拒否します。

- **検証・設計構造ブロック:** `bind`、`checker`、`config`、clocking ブロックと clocking drive、ユーザー定義プリミティブ（UDP）、`specify` ブロック。covergroup は一部に対応します。[カバーグループ](#covergroups)を参照してください。
- **union と配列・コンテナの混在型:** `tagged` union、unpacked union、packed union の代入パターン、`chandle` の固定長 unpacked 配列（`chandle a[4]`、typedef 経由を含む）、動的コンテナの固定長 unpacked 配列（`int a[2][$]`、typedef 経由を含む）、固定長 unpacked 配列を要素とする動的コンテナ（`int a[][2]`）。
- **制御フロー:** `disable fork` 以外の `disable` 文。宣言初期化子、`final`、`always_comb`、`always_ff` 内の `fork`（これらの文脈にはスケジューラ用のプロセス基盤がありません）。
- **時刻・システムタスク:** `wait_order`、および未実装の IEEE 1800-2023 / Annex D システムタスク・関数。Celox は標準が定義する全タスク・関数名を認識しており、未対応のものは未使用パラメータや関数本体に書かれた場合も含め名前付きで報告します（例: `$q_initialize`）。

## 部分対応の機能 {#partial-support}

部分的に対応している機能は、各表で**対応**する部分集合と**未対応**の範囲を併記します。

### ポート・ネット・`defparam`・ゲートプリミティブ {#ports-and-gates}

| 機能 | 対応 | 未対応 |
| --- | --- | --- |
| **ワイルドカード `.*` 接続**（[#442](https://github.com/celox-sim/celox/issues/442)） | SystemVerilog ソース内で定義されたモジュール（interface ポート付きを含む）への `.*` 接続 | SystemVerilog ソース外（Veryl など）で定義されたモジュールへの `.*` 接続 |
| **`ref` および `const ref` ポート（23.3.3.2）** | モジュール・program における ANSI / non-ANSI の `ref`・`const ref` ポート：packed・unpacked・構造体・enum・`string`・`real`・クラス・動的コンテナ・`event` 型の変数および unpacked 構造体メンバの参照渡し、多段チェーン、package 変数の実引数、`const ref` の読み取り専用検査、IEEE 1800-2023 6.22.2 の型等価性検査 | トップモジュールの `ref` ポート。unpacked 配列・動的コンテナの単一要素への接続。インスタンス配列の要素ごとの分割接続。上下限の異なる queue 間の接続。`ref` ポート（または参照先の変数）に対する手続き的 `force`/`release`・`assign`/`deassign`。Veryl と SystemVerilog の言語境界を跨ぐ `ref` ポート |
| **複数ドライバのネット（6.6.1–6.6.3）** | 単一モジュール内の `wire`、`tri`、`wand`、`triand`、`wor`、`trior` ネットへの複数継続代入（6.6.1–6.6.3 に従って解決。2値シミュレーションでは X/Z リテラル無し）。`wire`・`tri` ネットへの継続代入とネット宣言時の代入は駆動強度で解決（28.12） | モジュール境界を跨ぐ複数ドライバ（モジュール間のトライステートバスなど）。複数ドライバの `wand`、`triand`、`wor`、`trior` ネットの駆動強度（28.12.4）。複数ドライバの `tri0`、`tri1`、supply ネット。値依存ループを含む関数を呼ぶドライバ。2値シミュレーションでの X・Z リテラル付きドライバ |
| **未駆動ネットと `inout` ポート** | 下位モジュールの `inout` ポートで両側に継続ドライバが1つずつある場合（例: `assign io = oe ? d : 'z;`）。独立遅延付きネットの未駆動ビット（`z` を読む） | トップモジュールの `inout` ポート。独立遅延の無い未駆動の内部ネット（[#460](https://github.com/celox-sim/celox/issues/460)） |
| **ゲート・スイッチプリミティブ（Clause 28）** | 遅延・駆動強度の無い単一または1次元インスタンス配列の `and`、`nand`、`or`、`nor`、`xor`、`xnor`、`buf`、`not` | その他のゲート・スイッチプリミティブ（トライステートバッファ、スイッチ、プル電源など）。ゲートの遅延・駆動強度・多次元インスタンス配列（[#457](https://github.com/celox-sim/celox/issues/457)） |
| **`defparam` 代入** | `defparam` 文を含むモジュールの直接の子モジュールインスタンスのパラメータ上書き（generate ブロック内を含む） | 子インスタンス越しのパス（`a.b.P`）、`$root` 起点のパス、インスタンス配列要素、interface インスタンス、文自身のスコープのパラメータ |
| **`let` 宣言（11.12）** | モジュール・interface・program の項目としての宣言（各呼び出し位置で展開） | package・generate ブロック・手続きブロック内の `let`。本体が使う名前を再宣言するスコープでの呼び出し |

### 階層参照 {#hierarchical-references}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **変数・ネット・継続代入** | 子インスタンス（generate ブロックおよびインスタンス配列要素を含む）・自モジュール名・上位モジュール名・`$root` を起点とするネットと変数の読み取り、手続き的書き込み（unpacked 配列要素、`string` 変数・メソッドを含む）、packed 構造体変数や packed 構造体配列要素のメンバ参照、および他インスタンス変数を駆動する継続代入（対象モジュール自身が継続代入や `always_comb` で駆動していない場合） | 階層名経由でネットを駆動する継続代入、または対象モジュール自身が継続代入や `always_comb` で駆動している変数への階層継続代入。ポート接続内の階層参照。名前付きブロック。packed 構造体・その配列要素以外の別スコープ構造体メンバ。`real` 型およびクラス型の信号。generate ブロック内部から相対的に始まる参照 |
| **サブルーチン呼び出し（23.8）** | 呼び出し側モジュールの全インスタンスで単一の特殊化に解決される、他モジュールスコープの task・関数呼び出し | 仮引数・戻り値・参照変数が `real`・`shortreal`・クラス・`event` 型のサブルーチン。2つのモジュール間で互いのサブルーチンを呼び合うこと |
| **パラメータ（23.6）** | 1つ以下の `[N-1:0]` packed 次元を持つ他インスタンスの整数パラメータ/localparam（`u.N`）の実行時値としての読み取り（128ビット超の値は X/Z ビット不可） | 非整数または複数 packed 次元の階層パラメータ。X/Z を含む128ビット超の値。展開時定数式（A.8.4 `constant_primary`）としての階層パラメータ使用 |
| **サブルーチンの static 変数（`f.v`、23.6）** | `f` が単純名で見えるスコープ（そのクラスや基底クラスのメソッド、または同じ設計要素内のサブルーチン）からのアクセス | インスタンス・package・名前付きブロック経由のアクセス。unpacked 構造体型の static 変数 |
| **interface 本体からの上位参照（23.8）** | interface インスタンス本体内（virtual interface 経由で呼ばれる task/関数を含む）の未宣言名について、interface の全インスタンスで同じ型のオブジェクトを指す場合のインスタンス化側スコープからの解決 | interface ポート越しに到達した場合の上位探索。別 interface 本体内にある interface インスタンス内での上位探索 |

### interface {#interface-restrictions}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **宣言** | 値・型パラメータ、ヘッダの package import、`localparam`、`typedef`、変数、ネット、`assign`/`always_comb`/`always_ff`/generate 論理、関数、task、64段までの入れ子 interface インスタンス | interface 内の暗黙ネット・`enum`・モジュールインスタンス・マクロ・コンパイラ指令。再帰的特殊化や64段を超える入れ子。interface 配列のメンバに対する変数宣言初期化子。使用先モジュールと異なる既定寿命（`interface automatic`） |
| **modport と接続** | `input` / `output` modport メンバ、`import` 関数・task、汎用 `interface` ポートおよび `interface.mp` ポート、明示・暗黙の名前付き接続（`.bus(bus)`、`.bus`）、modport 無しポート越しの読み取り | `inout`・`ref`・式ポート・`export`・clocking modport。受け側ポートが modport を宣言していないのに modport を名指す接続。modport 無しポート越しの書き込み。modport 無しポートから non-ANSI モジュールへのメンバ接続 |
| **依存・名前・識別子** | 単一コンパイル順における通常のスコープ解決、および `$` を含まない通常識別子 | ポート越しに使う interface のパラメータ・定数・型・メンバ宣言内でのメンバ参照。別ファイルのモジュールや interface より前に出現するモジュールが使う `$unit` 項目への interface からの参照。暗黙の generate 名（`genblk1`）への参照と generate ブロック越しのパス（`g.h.x`）。interface インスタンス名・ポート名の再利用。interface 経由で取り込んだ package 名と他 package 項目・モジュール宣言との衝突。interface を含むデザインでの `$` を含む識別子または単純識別子でないエスケープ識別子（展開が `$` を予約するため） |

複数モジュールから interface の task/関数を呼ぶ際の static 変数の記憶域（[#3739](https://github.com/celox-sim/celox/issues/3739)）については、[標準準拠コードに影響する逸脱](#deviations-standard-code)を参照してください。

### virtual interface {#virtual-interfaces}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **宣言・パラメータ・アクセス** | 整数定数の値パラメータを持つ interface を含む `virtual ifc` / `virtual ifc.mp` / `virtual ifc#(P)`。変数・クラスプロパティ・サブルーチン引数/戻り値・配列/コンテナ要素・クラス型パラメータ値。変数の読み書き。interface の task/関数呼び出し *（記憶域については[標準準拠コードに影響する逸脱](#deviations-standard-code)参照）* | 型パラメータや整数定数以外の値パラメータを持つ interface の virtual interface。virtual interface 越しに到達する clocking ブロックや入れ子インスタンス。virtual interface 越しのネットへの書き込み。package やクラス内での virtual interface 越し interface 関数呼び出しの `$bits`（モジュール内では対応、[#4056](https://github.com/celox-sim/celox/issues/4056)） |
| **イベント制御と `wait`** | static 変数・`this` のプロパティ・static 変数に保持されたオブジェクトのプロパティが持つ virtual interface 越しの `@(...)` および `wait`。named event の待ち `@(v.e)` / `@(v.e or other_event)`（待ち開始時にハンドルが指すイベントを待機）とトリガ | 自動変数・サブルーチン引数・自動変数に保持されたオブジェクトのプロパティが持つ virtual interface 越しの `wait`。virtual interface 越しの named event と他種イベントを混ぜたイベント制御（`@(v.e or v.d)`） |

### program {#programs}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **program 宣言（Clause 24）** | ポートとパラメータを持ちインスタンス化された `program` ブロック（`initial` 手続きは reactive region セットで実行） | モジュール内に入れ子にした program。匿名 program。未インスタンス化 program の暗黙トップ扱い。program 内の clocking ブロック |

program の region と `$exit` に関する差異は[標準準拠コードに影響する逸脱](#deviations-standard-code)を参照してください。

### unpacked 構造体 {#unpacked-structures}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **記憶領域と文脈** | モジュール変数、手続きローカル変数、`static`/`rand`/`const` でないクラスプロパティ、動的コンテナ要素、サブルーチン引数（`ref` を含む）および戻り値 | モジュールポート、ネット、パラメータ。`static`、`rand`、`const` のクラスプロパティ |
| **スカラ・入れ子メンバ** | 整数型、enum、`string`、クラスハンドル、packed 構造体、入れ子 unpacked 構造体（各メンバは独自の2値/4値型を保持） | `real` / `shortreal` メンバ |
| **固定長配列メンバ** | モジュール変数と手続きローカル変数における、素の整数型または packed ベクタ型の固定長 unpacked 配列メンバ（多次元および typedef 経由を含む） | enum・packed 集約・`string`・ハンドル・unpacked 構造体型の固定長配列メンバ。固定長配列メンバの宣言初期値。固定長配列メンバを持つ構造体のポート・クラスプロパティ・サブルーチン引数/戻り値・動的コンテナ要素化 |
| **動的コンテナメンバ（7.2.1）** | 動的配列・キュー・連想配列メンバ（構造体とともにコピー） | コンテナメンバの宣言初期値。固定長次元と動的次元の混在（`int a[2][$]`、`int a[][2]`）。コンテナメンバを持つ構造体の `%p`。同構造体の固定長配列。他コンテナ内の構造体要素が持つコンテナメンバの変更（`q[i].m.push_back(x)`、`q[i].m[j] = x`、`q[i].m = x` — 読み取り専用） |
| **構造体の固定長配列** | 全リーフメンバが整数型であり、全リーフが2値のみまたは4値のみで統一され、かつ初期値宣言を持たない場合のモジュール変数・手続きローカル変数 | `string`・ハンドル・動的コンテナメンバ、2値と4値の混在、またはメンバ初期値を持つ構造体の配列。構造体配列のポート・クラスプロパティ・サブルーチン引数/戻り値化 |
| **メンバ初期値（7.2.2）** | モジュール変数、手続きローカル変数、クラスプロパティにおける定数整数初期化子（型宣言スコープで解決） | enum・非整数・配列/コンテナメンバの初期値。入れ子構造体全体のメンバ初期化子。既定値付き構造体型のポート・サブルーチン引数/戻り値・動的コンテナ要素化 |

### 手続き的 `force`/`release` と `assign`/`deassign` {#procedural-force-and-assign}

| 構文 | 対応 | 未対応 |
| --- | --- | --- |
| **手続き的 `force` / `release`（10.6.2）— 配置と対象** | `initial`・`always`（プロセスとして実行）・task 内。対象はモジュール変数/ネット（階層参照や `u.bus[2].ready` など interface インスタンス/配列メンバを含む）、定数ビット/部分選択・メンバ、および packed 整数型固定長 unpacked 配列の定数添字要素（または定数選択・メンバ）*（変数の選択や unpacked 配列要素への拡張は[標準外コードの許容](#deviations-nonstandard-code)参照。条件により `forced_array` / `forced_specialization` 警告を報告）* | 関数・`final`・`always_comb`・`always_latch`・`always_ff` 内。連結。非定数境界の選択。unpacked 配列スライス。`real`・`string`・ハンドル・unpacked 構造体対象。トップモジュール入力ポート。ホスト `Simulator::force` でも強制される対象。他インスタンスが interface ポート越しに書く interface メンバ。複数エッジ駆動プロセスが書く unpacked 配列。代入内遅延 NBA が書く変数。同じ `always_comb`/`always_latch`/`always @*` が読み書きする対象やプロセス版 `always_comb` が全体代入する対象。変数を読む式を持つ force と重なる部分の force |
| **手続き的 `force` / `release` — 右辺式** | 定数、モジュール/インスタンス信号、static task 変数/引数（`force` 実行時に読み取り）、定数引数の `$urandom`/`$urandom_range`（実行時と右辺変数変化時に再抽選） | 副作用のある式、サブルーチン呼び出し、定数引数 `$urandom`/`$urandom_range` 以外のシステム関数。手続きブロックローカルや automatic task 変数を参照する式 |
| **手続き的 `assign` / `deassign`（10.6.1）** | `initial` および `always`（プロセスとして実行）内で、モジュール全体の packed 整数変数を対象とする副作用・呼び出し無しのモジュール信号式 *（間接書き込み時の上書きタイミングは[標準準拠コードに影響する逸脱](#deviations-standard-code)参照）* | サブルーチン・`final`・`always_comb`・`always_latch`・`always_ff` 内。連結・`real`・`string`・ハンドル・unpacked・手続きブロックローカル対象。`always @*` も代入する対象。副作用・サブルーチン/システム関数呼び出し・手続きブロックローカル読み取りを含む右辺 |

インスタンスの入力ポートへの `force` は接続先ネットではなくそのポートを強制し、`force` を行わない別モジュールからの階層書き込みは上書きされません。また `force` された unpacked 配列への実行時添字書き込みは、その要素の強制中ビットを保持します。

### 遅延 {#continuous-assignment-delays}

| 形式 | 対応 | 未対応 |
| --- | --- | --- |
| **代入内タイミング（10.4.2）** | `initial` および通常の `always` プロセスでの整数定数遅延・時間リテラル付きノンブロッキング代入（`lhs <= #constant rhs`） | ノンブロッキング代入内イベント制御（`lhs <= @(e) rhs`）。定数でない遅延。`initial` と通常の `always` 以外での遅延ノンブロッキング代入 |
| **継続代入遅延（`assign #d`、`wire #d w = expr`）**（[#444](https://github.com/celox-sim/celox/issues/444)） | スカラまたは packed 整数型の代入先（定数ビット/部分選択を含む）に対する非負整数定数または時間リテラル1つの遅延（右辺はサブルーチン呼び出しを含まない整数式） | rise/fall/turn-off の遅延リスト。変数・負数・単位なし小数 real の量。連結先や集約先。動的選択先。非整数の代入先。サブルーチン呼び出しを含む右辺 |
| **独立ネット遅延（`wire #d w;`）** | static な継続代入または対応ゲートプリミティブで駆動されるスカラまたは packed 整数ネットへの非負整数定数または時間リテラル1つの遅延（未駆動ビットは `z` を読む） | インスタンスのポート接続内の遅延ネット。遅延付き `input` / `inout` ポート。unpacked または非整数の遅延ネット。動的選択先を持つドライバ |

### イベント制御と待ち {#timing-controls}

| 構文 | 対応 | 未対応 |
| --- | --- | --- |
| **一般的な `@(...)` イベント式** | 信号、`posedge`/`negedge`/`edge`、ビット/部分/動的選択、純粋関数呼び出しを含む一般式、`iff`（通常の `always`/プロセス内）、static な virtual interface | イベント式中の不純な関数呼び出し。static な task 引数の読み取り |
| **`@(...)` 内のクラスプロパティ** | `@(posedge h.p)` のような素のプロパティ項目（定数および待ちプロセス自身が書く変数だけを読む `iff` を伴う場合を含む） | 式の一部や関数経由、またはプロパティが保持するコンテナ内のクラスプロパティ。待ちプロセス以外から書かれる変数を読むプロパティ項目の `iff`。プロパティ項目と同じイベントリスト内の一般式項目、または1ビット変数以外の `iff` |
| **`wait (condition)`** | 一般式、static な virtual interface、およびクラスプロパティ（該当プロパティのストアで再評価。詳細は[イベント実行順序](#timing-semantics)を参照） | static な task 引数・`rand_mode()`・`constraint_mode()` を読む条件。task や副作用のある関数を呼ぶ、または時刻・乱数・mailbox・process を読むクラスプロパティ上の条件 |
| **fork 共有自動ローカル変数** | fork で共有する自動ローカル変数を監視するイベント式 | グローバル/static への混在依存、外部呼び出し、スクラッチ状態を保持する関数、または関数・シード/plusargs 結果・`$readmemh` などのシステムタスクから監視対象への暗黙書き込みを含む複合イベント式 |
| **`always_ff` のイベントリスト** | 単一の素の1ビット信号のエッジ（`@(posedge clk)`）、クロックと本体で検査される非同期リセットのエッジの組、および式・メンバ・ビット選択・`edge` イベント（時間付きプロセスとして実行。[実行モデル](#celox-execution-model)参照） | `iff` 修飾子（[#452](https://github.com/celox-sim/celox/issues/452)）。複数ビットのエッジ信号。非同期リセットを伴わない複数エッジ *（素の `always` では同じ形を時間付きプロセスとして実行可能）* |

### named event と mailbox {#named-events}

| 機能 | 対応 | 未対応 |
| --- | --- | --- |
| **named `event`（15.5）** | スカラ `event` 変数、クラスプロパティ、サブルーチン引数。`-> e`、`->> e`、`@e` / `@(e1 or e2)`（`obj.e` や virtual interface の `v.e` を含む）、`wait (e.triggered)`、`e1 = e2`、`null`、等値比較 | event の配列。event ポート。unpacked 構造体の event メンバ。event を返す関数。`always_comb`/`always_latch` 内のトリガ。遅延やイベント制御付き `->>`。信号変化やエッジと混ぜたイベント制御（`@(e or posedge clk)`）。他被演算子と混ぜた `triggered` の `wait` や関数越し読み取り。`wait_order` |
| **mailbox（15.4）** | 型パラメータ付き `mailbox #(T)` / `std::mailbox #(T)`（`T` は64ビットまでの整数型、`string`、またはクラス・`mailbox`・`semaphore`・`process`・`event` ハンドル） | 型パラメータ無しの `mailbox`。64ビットを超える整数や上記以外の要素型 |

### クラス {#classes}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **パラメータと特殊化（8.25）** | 値・型パラメータ（typedef 名を持つ enum/構造体/union、package や `$unit` クラス、クラス自身の4値/`string` パラメータの名前引き渡しを含む）。interface クラス（`implements` / `extends`） | 値・型パラメータとしての匿名 enum・構造体・union 型。モジュールや package の名前付き4値・`string` 定数のクラスパラメータ値渡し。package クラス特殊化の型引数としてのモジュール内クラス |
| **プロパティとメンバ参照** | 2値/4値 packed 整数、enum、`string`、クラスハンドル、`chandle`、virtual interface、named event、非 `const` 動的コンテナ、非 `static`/非 `rand`/非 `const` の unpacked 構造体（固定長配列メンバ無し）、および1次元固定長配列プロパティ（下段参照）。`[N-1:0]` 宣言プロパティのビット/部分選択読み（制約内では任意範囲の選択可） | `real` プロパティ。`const` な動的コンテナプロパティ。プロパティの packed 構造体メンバ。unpacked 構造体型プロパティのメンバ選択。制約外での `[N-1:0]` 以外で宣言されたプロパティのビット/部分選択読み |
| **固定長配列プロパティ** | 非 `static`・非 `randc` で非負 `int` 境界の1次元固定長配列プロパティ。要素アクセス、他の同種プロパティや直接指定された配列変数との代入/比較（非構造体要素）、`[0:N-1]` の制約 `foreach` および unpacked 構造体ロケータメソッド | `static` または `randc` な固定長配列プロパティ。2次元以上・負の下限・`int` 範囲外の境界。他の同種プロパティや直接指定配列変数以外との代入/比較（unpacked 構造体要素では配列変数自体不可）。キー付き代入パターン。配列項目を含む unpacked 配列連結。スライス。`ref`/`output`/`inout` 仮引数渡し。`[0:N-1]` 以外の範囲での制約 `foreach` や unpacked 構造体ロケータメソッド |
| **代入・引数・オーバーライド** | プロパティへのブロッキング代入。プロパティ全体の `input` または `ref` 引数渡し（[参照引数](#reference-arguments)参照）。非配列整数型のモジュール変数/ポートまたはクラスハンドルを読む、モジュールクラスによる外部クラス仮想メソッドのオーバーライド | プロパティへのノンブロッキング代入。`output` または `inout` 引数としてのプロパティ。モジュール外宣言クラスの仮想メソッドオーバーライドで、他の型（unpacked 配列など）のモジュール変数/ポートや static 変数を読んだり、モジュール変数を読むモジュール内サブルーチンを呼ぶこと |
| **`$cast`（6.24.2）** | クラスハンドル間の動的 `$cast` | クラスハンドルでない変数への `$cast` |

### ランダム化・制約・`randsequence` {#randomization}

| 機能 | 対応 | 未対応 |
| --- | --- | --- |
| **スコープ `randomize()` / `std::randomize()`（18.12）** | 制約無しの全体 packed 変数（任意幅、符号有無可、packed 構造体）および64ビットまでのスカラ enum（宣言済み既知値）。64ビット以下の整数変数への `with` 制約 | enum 等の非整数または64ビット超変数への `with` 制約。未解決値や X/Z 値を持つ enum。unpacked 配列、ネット、定数、選択、連結引数 |
| **クラス `randomize()` と `randc`（18.3–18.4、18.8、18.11）** | 2値および4値整数 `rand` プロパティ（制約無し任意幅、4値 `rand` は2値結果）、スカラ enum（宣言値）、64ビット以下2値整数の固定長/動的ランダム配列、継承プロパティ、`pre_randomize`/`post_randomize`、`rand_mode()`、`srandom()`、`get_randstate()`/`set_randstate()`、明示的メンバ指定、`randomize(null)`。1–16ビット2値整数および65,536メンバまでのスカラ enum の制約無し `randc` | 集約プロパティ（static/非 static）。`static` な `randc` プロパティおよび `randc` 配列。64ビット以下2値整数以外を要素とするランダム配列。オブジェクト・`string` のランダムプロパティ。16ビット超または65,536メンバ超の `randc` ドメイン。未解決値や X/Z 値を持つ enum |
| **制約ブロックと式（18.5）** | 1–64ビットスカラ整数プロパティ（任意宣言範囲の定数ビット/部分選択）、比較・論理/ビット演算・`+`/`-`/`*`。`if`/`else` と `->`（状態ガードは求解前に評価）。`inside`（スカラ値と閉区間）。ランダム配列上の `foreach`・`sum()`・定数位置要素（配列サイズは要素・関数呼び出しより先に解決、範囲外要素は 0）。`dist`（定数重み/定数範囲境界、`$` 型境界）。等価型の単一クラスプロパティ上の `unique`。`soft` 制約と `disable soft`。`solve ... before`。`constraint_mode()`・継承・オーバーライド。クラスハンドルの `==`/`!=`。1段のハンドルメンバ（`h.m`、`this.h.m`、`super.h.m`、`item.h.m`、static ハンドル、選択 `h.m[3:0]`/`hs[i].m[0]`）。`input` 引数を持つクラス関数呼び出し（18.5.11） | 制約付き enum または `randc`。制約内で状態として読む4値 static プロパティ・他オブジェクトの4値メンバ・4値配列要素。`rand` ハンドルから到達するオブジェクト内の static プロパティを名指す制約。`sum() with`。`foreach` 内の `soft`。`default :/`・非定数重み・非定数境界の範囲付き `:/` を持つ `dist`。他変数・配列・スライス上の `unique`。深いハンドル連鎖（`h.g.m`）。制約ループ・インライン制約・`rand` ハンドル先・他制約呼び出しの引数内での関数呼び出し。制約中の型パラメータ。`inside` 集合内の配列集合・X/Z ワイルドカード・`$` 境界 |
| **インライン `randomize() with { ... }`（18.7）** | 名前付き制約と同じ部分集合。無修飾受け手プロパティ、受け手ハンドル変数（`h.randomize() with { h.x > 0; }`、開始時1回読み取り）、呼び出し側変数と非ランダム受け手プロパティから計算されるスカラ入力（`buffer[i]`、`req.rw`、`memory[rsp.addr]`、`m.get_size()`）、および非プロパティの呼び出し側ローカル/引数への `local::` | 集約型の呼び出し側入力。型パラメータ入力。呼び出し側クラスのプロパティを指す `local::`。ランダム変数から計算される呼び出し側式（`rand addr` での `memory[h.addr]`）。無修飾の受け手 `static` プロパティ（同名の呼び出し側変数へフォールバックせず拒否） |
| **`randsequence`（18.17）** | 重み付きの非再帰生成規則、`rand join`（バイアス対応。[乱数](#random-numbers)参照）、および packed/スカラ型の `input` 仮引数/戻り値 | 再帰的生成規則。重みの後のコードブロック。`input` 以外の仮引数。unpacked・動的サイズ・`event` 型の仮引数や値 |

ソルバ方針、`soft`、`dist`、`solve before`、4値制約の挙動、制約内関数呼び出し、`rand join` のバイアス式については[乱数](#random-numbers)を参照してください。

### 動的コンテナ {#dynamic-containers}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **コンテナ形状と入れ子** | モジュール変数・ローカル変数・クラスプロパティとしての動的配列（`T a[]`）、キュー（`T q[$]`、有界 `T q[$:N]`）、連想配列（`T m[K]`）、および入れ子動的コンテナ（`int m[string][$]`、`T m[C][C][E]`） | 動的コンテナの固定長 unpacked 配列（`int a[2][$]`、typedef 経由を含む）。固定長 unpacked 配列を要素とする動的コンテナ（`int a[][2]`）。コンテナポート。`const` コンテナ |
| **要素型** | 任意幅の packed 整数、`string`、クラスハンドル、`chandle`、対応 unpacked 構造体（固定長配列メンバ・既定値無し）、入れ子動的コンテナ | `real` / `shortreal` 要素。named `event` 要素 |
| **連想配列キー型** | 任意幅の整数型（packed 構造体・enum を含む）、`string`、クラスハンドル、`process` ハンドル | ワイルドカード `[*]`。`real`、unpacked 型、その他の非整数/非ハンドルキー型 |
| **リテラル・`new[]`・代入** | 位置指定パターン、`{}` / 要素連結、互換動的配列/キューまたは代入パターン（スカラ/非コンテナ/非構造体要素では `'{default: v}` を含む）を取る `new[n]`、`default:` のみの連想配列リテラル（`'{default: v}`、7.8.6）、ブロッキング要素代入およびビット/部分選択書き込み（64ビット以下の整数、かつコンテナ/添字に副作用がない場合） | `new[n](fixed_array)`。`default:` と他キーを混在させた `new[]` パターン、またはコンテナ/unpacked 構造体要素への `new[]` `default:` パターン。明示添字項目や添字項目と `default:` を混在させた連想配列リテラル。スライス。キー付きパターン。連結内の連想配列。ノンブロッキング要素代入。コンテナや添字に副作用がある、または要素が `string`・ハンドル・コンテナの複合要素代入。unpacked 構造体要素のメンバへの直接書き込み（要素全体を代入してください） |
| **並べ替えと比較（7.12.2）** | `reverse` と `shuffle`（動的配列・キュー、および64ビット以下非構造体要素の1次元固定配列）。`sort` と `rsort`（`with` 無しの64ビット以下整数と `string`、および動的配列/キュー上での `with` 式付き unpacked 構造体・クラスハンドル・64ビット超要素）。64ビット以下要素のコンテナの `==`/`!=`/`===`/`!==` 比較（`string`/ハンドルメンバを持つ構造体を除く） | `with` 無しでの64ビット超要素・unpacked 構造体・クラスハンドルの `sort`/`rsort`。多次元・unpacked 構造体・64ビット超要素・`with` 式付きの固定長配列の並べ替え。64ビット超要素や `string`/ハンドルメンバ付き構造体の `==` 比較 |
| **縮約メソッド（7.12.3）** | 1次元配列変数または動的コンテナクラスプロパティに対する `sum`、`product`、`and`、`or`、`xor`（固定配列は64ビット超も可、動的コンテナは64ビット以下） | `with` 式およびイテレータ引数。多次元配列。64ビット超要素の動的コンテナ |
| **ロケータメソッド（7.12.1）** | 動的配列・キュー・連想配列・1次元固定配列に対する `find`、`find_index`、`find_first`、`find_first_index`、`find_last`、`find_last_index`（`with` 必須）、および `min`、`max`、`unique`、`unique_index`（`with` 有無可、64ビット以下整数または `string`）。`item` / 名前付きイテレータと `.index` | 64ビット超整数キーの連想配列に対する添字返却ロケータ。継続代入内、多次元固定配列上、`real` 要素上、または破棄文としてのロケータ呼び出し。この部分集合外の `with` / イテレータ構文 |
| **照会とサブルーチン仮引数** | 外側次元の `$size`、列の `$left`/`$right`/`$low`/`$high`/`$increment`、`$dimensions`。整数キューの `$bits(q.pop_front)`。要素型・キュー上限が一致する互換コンテナ変数を実引数に取る `input`/`output`/`inout` コンテナ仮引数 | `string` 値やクラス値キューの pop 結果に対する型照会（`$bits` など）。要素次元の照会および連想配列境界の照会。コンテナ型の関数戻り値。`output`/`inout` 実引数としてのクラスプロパティやコンテナ要素。要素型が仮引数と異なる実引数 |

### 文字列 {#strings}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **スカラ `string` 変数とメソッド** | 標準 `string` 演算・メソッド（`atoreal`/`realtoa` を除く）、階層参照（generate ブロック内を含む）、文字列上の `inside` / `case inside` 一致比較、`$display`/`$sformatf`、DPI-C の `string` 引数/戻り値、`$value$plusargs` の `%s` による `string` 出力先、およびファイル I/O（`$fopen`、`$fdisplay`/`$fwrite`、`$writememh`/`$writememb`、`$fgets`、`$ferror`、`$sscanf`/`$fscanf` の入力/書式） | `atoreal` / `realtoa`。文字列上の `inside` / `case inside` における値範囲項目（`["a":"z"]`）。`$value$plusargs` の書式引数としての `string` 変数（または `string` 出力先への `%s` 以外の変換）。VPI・VCD/FST 出力・TypeScript API における `string` 値 |
| **`string` の固定長 unpacked 配列** | 代入パターン、配列全体および部分配列のコピー、添字読み書き、非破壊メソッド、完全要素に対するブロッキング文字書き込みと `putc`/`itoa`/`hextoa`/`octtoa`/`bintoa` | ノンブロッキング文字書き込み。配列全体や部分配列に対するメソッド呼び出し。配列値の `string` サブルーチン引数・戻り値 |

### 参照引数 {#reference-arguments}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **修飾子（13.5.2）** | automatic サブルーチンおよびクラスメソッド（virtual メソッドを含む）の `ref` および `const ref` | `ref static` |
| **実引数** | 変数（スカラ、ベクタ、`string`、クラスハンドル、動的コンテナ、unpacked 配列、unpacked 構造体）、クラスプロパティ、固定長 unpacked 配列要素、unpacked 構造体メンバ | 動的配列・キュー・連想配列の要素。クラスプロパティの要素 |
| **`ref` 仮引数への操作** | 全体の読み書き、プロパティ束縛済み `ref` の `output`/`inout` 引数への引き渡し、コンパイル時および展開済みループ位置における配列要素 `ref` の選択 | クラスプロパティまたはそのメンバに束縛された仮引数への部分（ビット/部分選択）書き込み。実行時にしか位置が定まらない配列要素に束縛された仮引数の選択 |

### 遅延アサーション・制御タスク・サンプル値 {#sampled-values}

| 機能 | 対応 | 未対応 |
| --- | --- | --- |
| **遅延即時アサーション（`assert #0`、`assert final` など）** | 単一の表示タスクまたは重大度タスク呼び出しをアクションとする、モジュール項目および手続き内の遅延アサーション | アクションブロック内のユーザーサブルーチン呼び出しまたは複数文 |
| **アサーション制御タスク（20.11）** | 引数無しの `$asserton` と `$assertoff`（モジュール/デザイン内の全アサーションのチェックを停止・再開） | `$assertkill`、`$assertcontrol`、アクション制御タスク（`$assertpasson` など）、レベル・スコープ・アサーションを引数に取る呼び出し。package や `$unit` 内での呼び出し（package のサブルーチンやクラス内のアサーションは制御外）。`initial` とエッジ制御以外の別の手続きでも呼ばれる場合の `always_comb`/`always_latch` からの呼び出し。複数経路を持つ並行 sequence（`##[m:n]` や `or`）との併用 |
| **サンプル値関数（16.9.3）** | プロセス・組み合わせ論理・複数クロック sequence およびその呼び出し先における `$sampled`。クロック推論可能なエッジ駆動プロセスおよびその呼び出し先における `$past`、`$rose`、`$fell`、`$stable`、`$changed` | 明示的クロックイベント引数。推論可能なクロックが無い状態での使用。入れ子呼び出し（`$past($past(a))`）。unpacked 配列を読む被演算子。複数クロック sequence 内での `$past`/`$rose`/`$fell`/`$stable`/`$changed`、または並行アサーション内の関数呼び出しを伴う被演算子 |

### 並行アサーション {#concurrent-assertions}

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **指令と配置** | 明示クロック付きのモジュール・generate 項目としての `assert property`、`assume property`、`cover property`。明示クロック付きのプロセス・task 内手続き文としての `expect (property_spec) action_block`（16.17） | 手続き内の並行アサーション（プロセス内の `assert`/`assume`/`cover property`）。暗黙クロックまたは既定クロック |
| **単一クロック sequence と property** | Boolean property。2つの Boolean 式の `p iff q`（`\|->` の後件も含む。`a \|-> b iff c` は `a \|-> (b iff c)`）。`\|->` または `\|=>` で繋ぐ固定遅延 sequence（`a ##1 b`、最大1024ティック）。有限サイクル遅延範囲（`a ##[1:3] b`、先頭遅延含む）・`or`・`and`・`intersect`・`b throughout s`（最大1024通りのマッチ経路、property 全体または単一経路前件の後件、match 項目無し）。単一/複数サイクル sequence への `disable iff`（16.12）。組み込み整数型のローカル変数と match 項目（固定遅延モジュール/generate アサーション内） | 非有界サイクル遅延範囲（`##[1:$]`）。繰り返し（`[*n]`、`[->n]`、`[=n]`）。sequence 演算子 `within`。1ティックで絶対にマッチしない sequence 同士の `intersect`。含意前件・`expect`・ローカル変数/match 項目付きにおける複数経路 sequence。`expect` または複数クロック sequence 内の `disable iff`。複数サイクルまたは名前付き被演算子を持つ `iff`、連鎖 `a iff b iff c` |
| **複数クロック sequence（16.13）** | 値イベント制御（4値エッジ、`iff`、static virtual interface メンバを含む）によるクロック切り替えを挟み、単一経路の単一クロック部分 sequence を `##1` または `##0` で繋いだ property 全体（括弧によるクロッキンググループは64段まで入れ子可、16.13.3）。部分 sequence 内の `$sampled` | 含意内、`disable iff` 付き、またはローカル変数/match 項目付きの複数クロック sequence。クロックとしての named event。クロック切り替えを跨ぐ `##1`/`##0` 以外の演算子。部分 sequence 内の `$past`/`$rose`/`$fell`/`$stable`/`$changed`。64段を超える入れ子 |
| **名前付き property / sequence** | 同一モジュール内（generate 外）で仮引数を持たず、本体がこの対応部分集合内に収まる宣言。ローカル変数を持たない名前付き sequence の、他 sequence の被演算子としての利用（同クロックまたは先頭） | 仮引数付き宣言。別スコープまたは generate ブロック内の宣言。複数サイクル sequence 内で異なるクロックまたはローカル変数を持つ名前付き sequence のインスタンス化 |
| **アサーション内の関数呼び出し（16.6）** | 被演算子、`disable iff` 条件、match 項目値における純粋関数（副作用無し）の呼び出し（引数はサンプル値を受け取り、本体は現在値を読む、16.5.1） | 不純な関数（`output`/`inout`/`ref`/`const ref` 仮引数、非ローカル書き込み、保持 static 状態、副作用のあるシステム呼び出し、直接・間接の string/クラスメソッド呼び出しを含むもの）。DPI import 呼び出し。`$past`/`$rose`/`$fell`/`$stable`/`$changed` の被演算子内の関数呼び出し |
| **アクションブロック** | 時間無しのブロッキング文、報告/重大度タスク、および停止・ノンブロッキング代入・プロセス開始/制御・イベント発行を行わないサブルーチン呼び出し | 時間付きまたはノンブロッキングのアクション文。停止・イベント発行・プロセス制御を行う呼び出し |

`expect (property_spec) action_block`（16.17）は、実行中のプロセスが次のクロックイベントから1回だけ評価を行い、各ステップをサンプル値で判定した後、Reactive 領域で成功・失敗の文（既定は `$error`）を実行して続行します。複数経路を持つ sequence はいずれか1つの経路が一致した最初のティックで成功し、いずれの経路も一致し得なくなった最初のティックで失敗します（前件が外れた試行はそのティックで空虚な成功となります）。

### カバーグループ {#covergroups}

covergroup（19 節）は型です。モジュール、program、パッケージ、コンパイル単位、generate ブロックで宣言でき、クラスに埋め込めます（19.4）。`new` は covergroup の引数でインスタンスを構築します。`sample()` は、`with function sample` 宣言の引数でインスタンスをサンプルします。`get_inst_coverage()`、`get_coverage()`、`T::get_coverage()`、`$get_coverage()` はカバレッジを返します。結果は `Simulation::coverage()` で読みます（[カバレッジ](#coverage)を参照）。

カバーポイント（19.5）は任意のビット幅の整数型です。ラベル、独自の型、`iff` ガードを付けられ、ガードの式を先に評価します。式とガードは、covergroup の引数と `sample` の引数、埋め込み covergroup ならクラスのメンバ、スコープの名前の順に名前を解決します。ビンは、19.5.3 の自動ビン（列挙型のものを含む）か、値と範囲のビン（`bins b = {0, [1:3], [8:$]};`）です。ビンの値には、定数、covergroup の引数、それらを引数とする関数呼び出しを書けます。関数呼び出しは構築時に評価するため、同じ型のインスタンスでもビンが異なることがあります。定義内では `option.name`、`comment`、`per_instance`、`weight`、`goal`、`at_least`、`auto_bin_max` に、19.10 が定めるレベルで代入できます。値には `new` のスコープの任意の式を書けます。

**未対応**の列の構文は、covergroup の宣言位置ではなく、その `new` やメソッドを書いた位置でエラーになります。何も構築しない covergroup は宣言内容によらず受理し、その型の変数も受理します。いずれも、19 節の未対応分を追跡する [#4294](https://github.com/celox-sim/celox/issues/4294) を示します。

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **宣言（19.3、19.4）** | モジュール、program、パッケージ、コンパイル単位、generate ブロックの covergroup。クラスに埋め込んだ covergroup。covergroup 型の変数 | `covergroup extends`（19.4.1）。interface 内で宣言した covergroup（interface 自体がエラーになります）。checker 内やループ generate ブロック内で宣言した covergroup。モジュールが型パラメータとして受け取った covergroup 型の構築とサンプル（そのモジュールで変数は宣言できます） |
| **カバーポイント（19.5）** | 任意のビット幅の整数型のカバーポイント。ラベル、独自の型、`iff` ガードの有無は問いません | `real` 型のカバーポイント。列挙子に x や z のビットを持つ列挙型のカバーポイント。clocking ブロックの信号のカバーポイント（clocking ブロックは未対応です） |
| **ビン（19.5）** | 19.5.3 の自動ビン（列挙型のものを含む）。値と範囲のビン（`bins b = {0, [1:3], [8:$]};`）。値は、定数、covergroup の引数、それらを引数とする関数呼び出し | ビン配列（`bins b[]`、`bins b[N]`）。`default` と `default sequence` のビン。`ignore_bins` と `illegal_bins`。`wildcard` ビン。ビンのガード（`bins b = {1} iff (en);`）。`with` フィルタ。集合式のビン。許容差付きの範囲（`[a +/- b]`）。遷移ビン。整数型でないビンの値 |
| **クロス（19.6）** | — | すべての `cross` |
| **オプション（19.7）** | 定義内での `option.name`、`comment`、`per_instance`、`weight`、`goal`、`at_least`、`auto_bin_max` への代入（19.10 が定めるレベルで、値は `new` のスコープの任意の式） | `type_option` への代入。`option` のメンバのうち `detect_overlap`、`cross_num_print_missing`、`cross_retain_auto_bins`、`get_inst_coverage` |
| **サンプリング（19.3、19.8.1）** | `sample()` | clocking イベント（`covergroup cg @(posedge clk);`）やブロックイベント（`@@(begin f)`）付きで宣言した covergroup。sequence のマッチ項目内の `sample()` |
| **引数（19.3、19.8.1）** | covergroup の引数と、`with function sample` 宣言の引数 | covergroup やその sample メソッドの `ref` 引数。unpacked 型の引数。カバーポイントが読む covergroup の引数のうち、整数型、文字列、クラスハンドルのいずれでもないもの（`real` など） |
| **`let` 宣言（11.12）** | — | covergroup の名前より後では `let` を展開しません。引数の型や既定値、カバーポイントの式、ガード、ビンの値、オプションの値が `let` を参照する covergroup はエラーになります（covergroup または `sample` の引数が同じ名前を持つ場合は、その引数を指します） |
| **メソッド（19.8）** | `sample()`、`get_inst_coverage()`、`get_coverage()`、`T::get_coverage()`、`$get_coverage()`。埋め込み covergroup のメソッドを、その変数（クラス内の `cg`、`obj.cg`）に対して呼ぶこと | `start()`、`stop()`、`set_inst_name()`。`ref int` 引数付きの `get_coverage` と `get_inst_coverage`。カバーポイントとクロスのメソッド。`option` と `type_option` のメンバの読み書き。条件式など、ハンドルを返す他の式に対する埋め込み covergroup のメソッド呼び出し |
| **構築（19.3、19.4）** | covergroup の引数を取る `new`。クラスの `new` メソッド内での、埋め込み covergroup への `cg = new;` または `this.cg = new;` による代入 | covergroup ハンドルを渡す浅いコピーの `new`（8.12）。`new` メソッド内での、クラスの他のハンドルを通した埋め込み covergroup への代入（`other.cg = new;`）。このメソッドの外での代入は、19.4 が定めるとおりエラーです |
| **型名（20.6.1）** | — | covergroup 型、その変数、それを含む配列や構造体の `$typename`（20.6.1 にはその名前の規則がありません） |
| **結果（19.9）** | `Simulation::coverage()` と `Simulator::coverage()`（[カバレッジ](#coverage)を参照） | カバレッジデータベース。[カバレッジデータベースはありません](#deviations-standard-code)を参照してください |

### I/O・書式出力・システムタスク {#formatted-output-to-strings}

実行時 I/O（`$fopen`、ファイルへの `$display`/`$write`、`$system`、plusargs、`$sformatf` 系など）は時間付き `Simulation` プロセスを必要とし、組み合わせプロセスや継続代入からの呼び出しは拒否されます。`$system` は WebAssembly ホストでは未対応です。`$readmemh` / `$readmemb` については[メモリファイル](#memory-files)を参照してください。

| 機能 | 対応 | 未対応 |
| --- | --- | --- |
| **`$strobe` / `$fstrobe`（21.2.2、21.3.2）** | スケジュール済みプロセス内のモジュール/static 式（`b`/`o`/`h` 形式を含む） | サブルーチン内の呼び出し。引数内の手続きローカル、展開済みループ変数、更新、または副作用のある呼び出し |
| **`$monitor` / `$fmonitor`（21.2.3、21.3.2）** | スケジュール済みプロセス内の static スカラ変数全体、コンソール `string` 変数（`$monitor`）、定数、および直接の時刻照会 | 配列要素、スライス、一般式、`$fmonitor` での `string` 変数、および上記 `$strobe` と同じ呼び出し文脈制約 |
| **`$fopen`、`$fdisplay`/`$fwrite`、`$writememh`/`$writememb`** | パス・モード・書式引数に対する UTF-8 文字列リテラル、packed 整数文字列、および `string` 変数。static unpacked 配列への `$writememh`/`$writememb` | 非 UTF-8 のファイル名やモード。動的配列や連想配列への `$writememh`/`$writememb` |
| **`$fgets`（21.3.4.2）** | 書き込み可能な `string` 変数または選択した固定長文字列配列要素 | packed 整数バッファ、クラスプロパティ、または動的コンテナへの出力 |
| **`$ferror`（21.3.7）** | `string` 変数、選択した固定長文字列配列要素、または640ビット以上の packed バッファ | クラスプロパティまたは動的コンテナへの出力 |
| **`$sscanf` / `$fscanf`（21.3.4.3）** | 整数変換（`%d`、`%h`/`%x`、`%o`、`%b`、`%c`）および実数変換（`%e`/`%f`/`%g`）。入力と書式に対する文字列リテラル、packed 整数、`string` 変数 | `%s` 文字列出力。`%t` 時間スケーリング |
| **`$sformat` / `$swrite`（21.3.3）** | `string` および packed 整数への出力（`$sformatf` は `string` を返却） | `byte` の unpacked 配列への出力 |
| **`%p` 書式出力（21.2.1.6）** | 整数、実数、文字列、クラスハンドル、packed 構造体（副作用のある呼び出し無し）、unpacked 構造体変数/メンバ/クラスプロパティ（動的コンテナメンバ無し）、enum、および手続き本体内のスカラ/unpacked 構造体/enum のコンテナ（動的配列・キュー・連想配列。整数要素は64ビット以下、連想キーは整数・`string`・スカラ enum） | 関数呼び出し（または副作用を伴う呼び出し）やキュー pop から得た enum 値。packed union。関数戻り値または副作用呼び出しから得た packed 構造体。enum の packed 配列。固定長 unpacked 配列。`real` や動的コンテナメンバを持つ構造体。集約型の関数戻り値。64ビット超要素・`real` 要素・他集約要素を持つコンテナ、または packed enum・クラスハンドルキーの連想配列 *（実行時書式の `%p` については[標準準拠コードに影響する逸脱](#deviations-standard-code)を参照）* |
| **`$test$plusargs` / `$value$plusargs`** | packed 整数または文字列リテラルによる照会。packed 整数への整数変換、および packed 整数または `string` 変数への `%s` 変換 | `real` 変換。照会/書式引数としての `string` 変数。`string` 出力先への `%s` 以外の変換 |
| **時刻照会・`$timeformat`・`%t`** | `$time`、`$stime`、`$realtime`、`$timeformat`（20.4.3、最大4,096桁/幅。[時刻と精度](#time-and-precision)参照）、数値に対する `%t`。モジュール/インスタンスに対する `$timeunit`、`$timeprecision`、`$printtimescale` | `string` 値への `%t`。選択したインスタンス配列要素や generate パスへの `$timeunit`/`$timeprecision`/`$printtimescale`。デザインの全体時間精度より細かい時間単位を持つスコープ |

## 個別制約 {#narrow-limitations}

全体としては標準に従う機能における、個別の制約は次のとおりです。

- **デザイン構造:**
  - `module m({a, b}, c[3:0], .p(d))` のような non-ANSI ポート式（[#1953](https://github.com/celox-sim/celox/issues/1953)）。ポート名のヘッダと本体宣言による素の non-ANSI ポートは対応します。
  - 非定数の ANSI `input` ポート既定値（23.2.2.4）、`input` 以外のポートの既定値、および unpacked 構造体ポートの既定値（定数のスカラ/単一 `input` ポート既定値は対応。定数の unpacked 配列 `input` ポート既定値は `unpacked_port_default` で警告し strict モードで拒否）。
  - generate ブロック内インスタンスのポート接続が宣言する暗黙ネット（6.10）: これを読む入力接続は拒否し、出力だけが駆動するものはブロックではなくモジュールのネットになります。
  - 多次元のインスタンス配列、負の下限を持つインスタンス配列、または4,095要素を超えるインスタンス配列。
  - package ネット（[#1146](https://github.com/celox-sim/celox/issues/1146)）。package 変数はクロックやリセットにできません。
  - ワイルドカード import（26.3）: モジュール・interface・program・package スコープ、generate ブロック、手続きブロックは、参照位置ごとではなく本体全体で名前を1度だけ束縛します（`export p::x;` は package 内で束縛）。1つの束縛対象内で参照位置により異なる宣言を指す場合は拒否します（[#4646](https://github.com/celox-sim/celox/issues/4646)）。先行する参照がワイルドカード import で取り込んだ名前は、同名を宣言する別パッケージの後続ワイルドカード import の後でも、規格どおりその宣言に束縛されたままです。また、ブロック自身のワイルドカード import を通じて package 変数を読むブロックローカル初期化子も拒否します（[#4350](https://github.com/celox-sim/celox/issues/4350)）。
  - `$unit` 変数を宣言できるソースファイルは1つだけです。
- **型と式:**
  - enum の packed 配列の要素に対する enum メソッド（`first`、`next` など）（[#2417](https://github.com/celox-sim/celox/issues/2417)）。
  - ベクタ範囲外に出る定数部分選択の読み取り、および配列要素や内側 packed 次元の宣言範囲を出る定数部分選択の書き込み（[#88](https://github.com/celox-sim/celox/issues/88)）。素の packed ベクタへの一部範囲外定数書き込み（例: `logic [7:0] q` への `q[9:6] = v` で範囲内の `q[7:6]` のみに書く）と全ての実行時選択は標準に従います。
  - packed 構造体のメンバである構造体の packed 配列における、要素メンバへの実行時部分選択および要素選択（`s.regs[i].q[j +: 4]` など）。
  - パラメータ式中の縮約演算子（[#456](https://github.com/celox-sim/celox/issues/456)）。
  - 型演算子（6.23）: 型参照と値の比較、型に対する `case` の範囲項目、ポートを読む式や generate ブロック内の式への `type(...)` 適用。
  - `$typename` <a id="type-names"></a>（20.6.1）: 一意な綴りを持たない型に対して処理系生成名を作らず拒否します。匿名 enum・構造体・union、soft/tagged union、unpacked 配列メンバを持つ unpacked 構造体、enum や符号付き typedef の packed 配列、整数でない値パラメータを持つクラス、`$typename(a + b)` のような名前やデータ型でない被演算子が該当します。20.6.1 に規則のない virtual interface 型は宣言どおり出力します（`virtual bus_if`、`virtual bus_if.mp`、`virtual bus_if#(8, 4)`）。
  - 展開時定数としての集約型パラメータ: unpacked 配列型のパラメータは開始時から値を持ちます。1次元配列パラメータの要素（および要素型が符号付き名前付き型の packed 配列でない限りその packed 次元の選択）が展開時定数になるのは、要素が127ビット以下かつ X/Z 無しの packed 整数で配列要素数が4,096以下の場合のみです。packed 構造体パラメータのメンバは127ビット超または X/Z を含まない限り展開時定数です。配列全体および上記以外の要素・メンバは展開時に必要な定数としては使えません。
- **プロセスとサブルーチン:**
  - `always_comb`（[#453](https://github.com/celox-sim/celox/issues/453)）および `always_latch`（[#431](https://github.com/celox-sim/celox/issues/431)）内のノンブロッキング代入。ブロッキング代入を使ってください。
  - static 引数を持つ package および `$unit` の時間付き task。
  - 組み込み `process` クラスの構築・継承。関数からの自スレッド停止や `await()` 呼び出し。
  - ループ: 手続きループの展開は最大10,000回。実行時に回数が決まる組み合わせループは数え上げ `for` のみ。1モジュールあたりの loop-generate 展開総数が1,000,000回を超えた場合、ビット複合代入による genvar 更新（[#455](https://github.com/celox-sim/celox/issues/455)）、および generate 中の再帰的定数関数展開は拒否します。
  - `final` 手続き <a id="final-procedures"></a>: タイミング制御、ユーザー task 呼び出し（関数経由を含む）、戻った後にイベントをスケジュールする関数（13.4.4）は拒否します。`final` 内で宣言した切り離された `fork … join_none` の子は初期化されますが開始しません（[実行モデル](#celox-execution-model)も参照）。
  - 名前付きサブルーチン引数: 選択添字に手続き的副作用のある呼び出し。受け手やオブジェクトを読む先行引数に副作用がある場合の、`always_comb` や `assign` 内で受け手を使うメソッド既定値。
  - 他インスタンスのコードで実行されるサブルーチン（階層呼び出し、仮想インターフェースやインターフェースポート経由の呼び出し、モジュール内クラスのオーバーライド）内の `%m`（21.2.1.5）は、そのサブルーチン自身のインスタンスを示します。ただし、データメンバを持たないインターフェースのポート経由でインポートしたサブルーチンと、モジュールの項目を読まないモジュール内クラスのオーバーライドでは、呼び出し側インスタンスを示します（[#4642](https://github.com/celox-sim/celox/issues/4642)）。

## 意図的な逸脱 {#intentional-deviations}

### 標準準拠コードに影響する逸脱 {#deviations-standard-code}

有効な IEEE 1800-2023 コードを受理しつつ、標準と意図的に異なる挙動をとる項目です。移植性が必要なコードはこれらに依存しないでください。

- **program の継続代入は Reactive region で実行しません。** program 内の
  継続代入はデザインの組み合わせ論理と評価し、program が書くデザイン論理は
  reactive region セットの実行中に安定します。そのため `#0` の後に再開した
  program は新しい値を読みます。IEEE 1800-2023 4.5 では reactive region セットが
  空になるまで評価しません。Icarus、Verilator、xezim も Celox と同じです。
- **モジュール跨ぎおよび virtual interface 経由で呼ぶ interface サブルーチンの記憶域。**
  複数モジュールの interface ポート経由で呼ぶ static な関数・task の static 変数、
  および中断し得る task の static 仮引数は、interface インスタンスごとではなく
  呼び出し側モジュールごとに保持します（13.3.2、13.4.2、[#3739](https://github.com/celox-sim/celox/issues/3739)）。
  インスタンス自身経由と単一モジュールポート経由の呼び出しは1つを共有します。
  同様に、virtual interface 経由で呼ぶ interface の task・関数（`vif.drive(req)`、25.9）は
  ハンドルが指すインスタンスに対して実行しますが、static 変数は呼び出しを行うスコープごとに保持し、
  仮引数は static なサブルーチンでも呼び出しごとに持ちます。
- **手続き的 `assign` における間接書き込みの上書きタイミング。** 手続き的 `assign` が
  有効な間、通常の代入文以外による変数への書き込み（`output` 引数、連結先、遅延 NBA、
  他モジュールの階層名）は抑止ではなくそのタイムステップ内で上書きされるため、
  変数を待つプロセスが書き込まれた値を観測する場合があります。
- **実行時書式の `%p` は文字列以外の値を簡略化します。** `%p` 変換がコンパイル時リテラルではなく
  実行時に評価される書式文字列から来る場合、Celox は `string` を引用符付きで、それ以外の値を
  `%0d` として出力します。
- **カバレッジデータベースはありません。** `$set_coverage_db_name` と
  `$load_coverage_db` は引数を評価してコンパイル警告
  （[`[diagnostics]`](./celox-toml.md#diagnostics) の `no_coverage_database`）を
  出します。何も保存せず、読み込みません（19.9、
  [#4294](https://github.com/celox-sim/celox/issues/4294)）。covergroup のカバレッジは
  `Simulation::coverage()` で読みます。即時 `cover` は pass 文を実行します。
  アサーションカバレッジと 40 節のコードカバレッジは収集しません。
- **program 外の `$exit` は警告します。** program スレッド以外からの `$exit` は
  24.7 どおり無視しますが、Celox はコンパイル警告（[`[diagnostics]`](./celox-toml.md#diagnostics) の
  `exit_outside_program`）も出します（未使用関数の宣言内呼び出しも含みます）。
- **UVM の DPI 関数は組み込みです。** UVM の `src/dpi` の C 名での import は
  ユーザー C 関数ではなく Celox の実装に束縛します。[DPI-C imports](#dpi-c-imports)を参照してください。
- **オプトインの互換モード。** [`[compatibility]`](./celox-toml.md#compatibility) の
  `legacy_empty_reductions` や `shared_always_temporary` などのオプションは、
  既定では IEEE 1800-2023 準拠の挙動を保ち、明示的に有効化したときだけ逸脱します。

### 標準外コードの許容 {#deviations-nonstandard-code}

Celox が意図的に受理している標準外構文および互換拡張です。移植性が必要なコードはこれらに依存しないでください。

**既定で警告、strict モード（`strict = true` / `--strict` / `.strict(true)`）で拒否:**

- **初期化付きローカル変数での `static` キーワード欠落を許容（6.21）。**
  IEEE 1800-2023 6.21 は初期化値を持つ static 変数の宣言に明示的な `static` を要求します。
  Celox は定数初期化子ではどの手続きでも、実行時初期化子では `initial` のような
  1度だけ実行する手続きでキーワード無しの宣言も受け付け、`implicit_static_initializer` 警告
  （既定 `warn`、strict mode で `error`）を出します。`always` 内の実行時初期化子に
  `static` も `automatic` も無い場合は拒否します。
- **時間尺度の有無が混在する場合の既定値フォールバック（3.14.2.3）。**
  一部の設計要素だけ時間単位・精度（宣言・継承・`` `timescale ``）を持ち他が持たない場合に
  エラーとせず、`mixed_timescale` 警告（strict mode で `error`）を出して該当要素に
  1 ns / 1 ps を割り当てます。
- **その他の既定警告な互換診断。** 重複モジュール宣言（`duplicate_module`）、
  固定長 interface 配列への `$size`（`interface_array_query`）、interface 配列への
  `bus.WIDTH` 省略記法（`interface_array_parameter`）、定数 unpacked 配列の `input` ポート既定値
  （`unpacked_port_default`、23.2.2.4）、`always_ff` と他エッジプロセスによる
  メモリ共有（`multiple_always_ff_drivers`）は、既定で警告を出し strict mode で拒否します。
  詳細は [`[diagnostics]`](./celox-toml.md#diagnostics)（および `inferred_latch`）を参照してください。

- **固定長 unpacked 配列への packed 値と一括充填の代入（7.6）。**
  `packed_to_unpacked_assignment` が既定の `warn` または `allow` の場合は、
  配列全体と同じ幅の packed 値をビットごとに代入したり、`'0` のような
  unbased unsized リテラルで全要素を埋めたりできます。strict モードでは、これらの代入と
  同等のモジュールポート接続（10.8）を既定で拒否します。配列型への明示的なキャストや
  `'{default: '0}` のような代入パターンを使ってください。
- **変数の選択や unpacked 配列要素への手続き的 `force` と `release`（10.6.2）。**
  変数の定数ビット選択・部分選択・メンバ、および packed 整数型の固定長 unpacked 配列の
  定数添字要素（またはその要素の選択）に対して、`nonstandard_force_target` を既定で警告し、
  strict モードではエラーにします。配列要素への `force` では、制御信号のサイズを示す
  `forced_array` 診断も出ることがあります。
- **ベンダー拡張の別名 `$psprintf`（21.3.3）。** Celox は `$sformatf` として扱い、既定で
  `psprintf_alias` を警告します。strict モードでは既定で拒否します。

## 処理系定義・順序未規定の挙動 {#semantics-worth-knowing}

IEEE 1800-2023 が処理系に委ねる点、順序を指定しない点、非決定的とする点について、
Celox は次のように選びます。移植可能なコードはこれらに依存しないでください。

### 定数の幅 {#constant-widths}

IEEE 1800-2023 は、ベクタの幅の上限を 65,536 ビット以上の範囲で処理系に委ねます（6.9.1、7.4.1）。
パラメータ、それを作る連結、その部分選択などの定数値は最大 1,048,576 ビットまで許容します。
複製、ビットごとに組み立てる定数、定数関数呼び出し内の値は最大 65,536 ビットです。
これを超える定数は構築前に拒否します。

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
- `$timeformat`（20.4.3）は以降のすべての出力および `$sformatf` における `%t` 変換の
  単位・小数点以下桁数・接尾辞・最小フィールド幅を設定します。丸めは四捨五入（0から離れる方向）で、
  `%0t` などの幅指定は最小フィールド幅を置き換えます。接尾辞は NUL 文字を含む文字列リテラルです。
  これを呼ぶ `always_comb`・`always_latch`・`always @*`（サブルーチン経由を含む）はプロセスとして実行し、
  時刻 0 で1度（`always @*` を除く）と読み取り対象の変化ごとに動きます。
  実行時に単位番号が 2〜-15 の範囲外になるか精度・幅が負になると実行時エラーで終了し、
  定数の場合は解析エラーになります。小数点以下桁数とパディング幅の上限は 4,096 です。
- コンパイル単位（`$unit`）で宣言されたクラスやサブルーチンは、コンパイル単位の
  時間単位と時間精度を使用します。`` `timescale `` 指令はこれらを設定しないため（3.14.2.3）、
  `$unit` 内で `timeunit`/`timeprecision` を宣言しない限り既定の 1 ns / 1 ps になります
  （Verilator は最後の `` `timescale `` を適用します）。移植可能なコードでは `$unit` に
  `timeunit` を宣言してください。全体の時間精度より細かい精度は整数ステップへ丸められます。
- `final` 手続きは決定的ですが順序未規定の順に逐次実行します。コンストラクタや
  関数を呼ぶ package・モジュール変数初期化子の相対順序も未規定です（6.8、10.5）。
  依存しないでください。

### 乱数 {#random-numbers}

- `$urandom` は決定的な32ビット線形合同生成器を使います（18.13.1、18.14）。
  既定シードと系列は処理系定義で、他シミュレータと一致する必要はありません。
  `$random` と `$dist_*` は Annex N に従うため、同附属書の他実装と一致します。
  `process` およびクラスオブジェクトの乱数状態文字列（`get_randstate`、18.13.4）は
  不透明で Celox 固有です。`set_randstate`（18.13.5）は `get_randstate` が返した文字列だけを
  受け付け、それ以外ではシミュレーションをエラー終了します。
- 組み合わせ手続きでは、プロセス活性化時に乱数を引きます。入力不変の
  再評価ではストリームは進みません。
- 制約付き `randomize()` はオブジェクトのシードで充足代入を探索し、選択した
  全プロパティをまとめて更新します。一様分布は保証しません
  （[#2187](https://github.com/celox-sim/celox/issues/2187)）。
  不充足が証明された場合は `0` を返し、プロパティを保持し、post フックを
  省略します（18.6.3）。資源枯渇は実行時エラーです。
- `soft` 制約（18.5.13）は優先度の高い順から試み（18.5.13.1）、ハード制約と
  先行して維持した `soft` 制約と両立すれば維持し、そうでなければ破棄します。
  `disable soft` はその変数を名指す下位の `soft` 制約を破棄します（18.5.13.2）。
  各段階の探索はシードを保持するため選択は再現可能です。`soft dist` は重み非ゼロの項目に
  被演算子を制約し、維持されている間はハードな `dist` と同様に重みで選択を偏らせ（18.5.3）、
  破棄されると影響を与えません。
- 4値 `rand` プロパティは2値の結果をとります。4値状態プロパティ、4値型の制約内関数戻り値、
  インライン制約や `std::randomize() with` の4値呼び出し側入力に含まれる X/Z ビットは各演算の
  4値規則（11.4）に従います。X/Z が制約やガードの真偽を決める場合は実行時エラー（18.3）となり、
  マスク・選択・既知 0 が決める `&&` などで真偽を決めない場合はそのまま値を使います
  （そうしたガードは求解前に評価します、18.5.13）。真偽がランダム変数にも依存する場合は、
  見つかった解が到達するかどうかを決めます。
- `solve before`（18.5.9）は解の集合を変えず、探索の選択順序にも影響しません。循環順序の診断も行いません。
- 制約が呼ぶ関数（18.5.11）は通常の呼び出し（オブジェクトのクラスによるオーバーライドを含む）として実行します。
  引数内のランダム変数がそれらだけを含む制約で解かれた後に呼ばれ、戻り値は他の制約に対する状態入力となるため、
  他の制約により解無しになることがあります。引数でないランダム変数を関数が読む場合は呼び出し前の値を読みます。
- `randsequence`（18.17）: 複数の生成規則は重み（省略時は 1）に従ってスレッドの乱数生成器から1つを選びます（18.14.2）。
  `rand join` は同じ乱数生成器を使って項目をランダムに織り交ぜます。18.17.5 は bias に数式を定めていないため、
  Celox は bias を1度評価し、各ステップで確率 |2 × bias − 1| で残項目が最少（bias < 0.5）または最多（bias > 0.5）の規則を進め、
  それ以外は残項目のある任意の規則を等確率で進めます（0.5 は偏り無し、0.0 と 1.0 はそれぞれ最短・最長を常に優先）。
- 配列の `shuffle` は呼び出しプロセスの乱数ストリームから引きます。

### カバレッジ {#coverage}

`Simulation::coverage()` と `Simulator::coverage()` は、インスタンスを持つ
covergroup 型ごとに、インスタンス、サンプル数、`$get_coverage` が返すカバレッジを
報告します。レポートを読んでも状態は変わらず、時刻も進みません。

- **covergroup 型はエラボレートしたスコープに属します。** パラメータ値が同じ
  モジュールインスタンスは、typedef と同様に、モジュールが宣言する covergroup 型を
  共有します。6.22 はインスタンスごとの型とします。パラメータ値が異なる
  エラボレーションはそれぞれの型を宣言し、レポートは同じ名前で並べます。
  パッケージの covergroup はデザインに 1 つの型、コンパイル単位の covergroup は
  ソースファイルごとの型です。
- **`null` への `sample()` は何もサンプルしません。** コンストラクタが作らなかった
  埋め込み covergroup（19.4）でも、それ以外の covergroup 変数でも同じです。
  ハンドル経由の `get_coverage()` は型のカバレッジを返し、ハンドルを読みません。
  `null` への `get_inst_coverage()` は実行時エラーです。
- **項目のない covergroup にカバレッジはありません。** カバーポイントもクロスも
  宣言しない covergroup の `get_inst_coverage()` と `get_coverage()` は 0 で、
  `$get_coverage()` には寄与しません。寄与する covergroup がデザインに無いとき、
  `$get_coverage()` は 100 です（19.11）。
- **ビンの値はカバーポイントの型の値として比較します（19.5.7）。** 型が持たない
  値はビンに含まれず、範囲は型が持つ値の分だけ含まれます。定数の場合はビルド時に
  警告します（`covergroup_bin_value`）。covergroup の引数で決まる値の場合は、
  インスタンスを構築する `new` が、そのインスタンスの該当する値をまとめた
  `$warning` レポートを出します。1 未満の `option.auto_bin_max` も同じレポートで
  警告し、自動ビンは 1 つになります。両端の符号が異なる範囲は、二項演算子の
  オペランドと同じく共通の型で読みます（11.8.1）。`[-1:8'd5]` の -1 は大きな符号
  なしの値になり、これも警告します。符号付きのカバーポイントでは、符号なしの値は
  そのビット列が表す値になります。8 ビット符号付きのカバーポイントで `8'hFF` は
  -1 です。
- **x と z。** x や z のビットを含むサンプルは、どの自動ビンにも入りません
  （19.5.3）。4 値型のカバーポイントでは、x や z を含むビンの値は同じビットを持つ
  サンプルだけに一致します。2 値型のカバーポイントでは、その値はビンに含まれず、
  警告します（19.5.4 と 19.5.7 はここで食い違います）。x や z を含む範囲の端は、
  その範囲ごと除きます。2 値シミュレーションでは、サンプルに x や z は現れません。
- **符号付きのカバーポイントの自動ビンは最小値から始まります。** 自動ビンの名前は
  値で表します。`auto[3]`、`auto[0:3]`、列挙子なら `auto[NAME]` です。ラベルの
  ないカバーポイントは、式が変数名だけのときに限り、その変数名を名前とします
  （19.5）。オブジェクトのメンバ（`t.kind`）や変数の選択を含め、それ以外の式の
  カバーポイントは `$point<n>` と表示します。`n` は covergroup 内の
  カバーポイントの番号で、1 から数えます。
- **カバーポイントのビンは 1,048,576 個までです。** これを超える構築は
  シミュレーションをエラーで終了します。ヒープがその型のカバーポイントの値として
  受け取れないサンプルも同じです。これは Celox の誤りでしか起きず、そのサンプルは
  数えません。
- **サンプルはカバーポイントの関数を 1 回だけ呼びます。** `sample()` は、すべての
  カバーポイントのガードと式を宣言順に評価してから数えます。そのため、式の中の
  関数が `sample()` を呼んでも構いません。
- **カバレッジは累積し、インスタンスは解放しません。** どこからも参照されなく
  なったインスタンスのサンプルは、型のカバレッジとレポートに残ります。レポートは
  そのインスタンスを生存していないものとして示します。トランザクションごとに
  covergroup を構築するテストベンチでは、レポートのインスタンスが毎回 1 つ増えます。
- **covergroup の構築は乱数を引きません。** 手続きやクラスのコンストラクタに
  covergroup を足しても、その乱数スティミュラスは変わりません（18.14.1）。
  モジュール内で初期化子付きで宣言した covergroup 変数は、初期化子付きの
  クラス変数と同じく独立したプロセスで、プロセスにシードを与える順序の 1 つを
  占めます。
- **カバレッジはチェックポイントに含まれ、状態ファイルには含まれません。**
  `Simulation::restore` はチェックポイントのカバレッジを復元します。covergroup を
  構築した後の `save_state` と `load_state` は `StateError::Coverage` で失敗します。
- レポートのインスタンス名は `<型>#<n>` で、型ごとに構築順に 0 から番号を
  振ります。

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
- **covergroup。** モジュールや program 内で初期化子付きで宣言した covergroup 変数
  （`cg c = new;`）は時刻 0 のプロセスが構築するため、`Simulation` が必要です。
  そのデザインを `Simulator` で構築すると、他のプロセスと同じく失敗します。
  `Simulator` ではクロック同期の手続きで構築してください
  （`always @(posedge clk) begin if (c == null) c = new; c.sample(); end`）。
  モジュールの covergroup 変数は、クラスハンドルと同じくデザイン状態内のハンドルです。
  このとき `Simulation` のコンパイル済みスケジューラは、変数を何も構築しなくても、
  タイムスロットの終わりごとにホストを呼び出します。この呼び出しを持たない環境では、
  そこでホストへ戻ります（x86-64 は呼び出しを持ちます）。`Simulation` では、
  クロック同期ロジックからのサンプルが加えるのは、サンプルの呼び出しだけです。
  `Simulator` では、
  クロック同期の手続きが covergroup を構築またはサンプルするクロックは、クロック
  ドメインを 1 回実行するだけの `tick` も使えなくなります。クロック同期ロジックからの
  他のホスト呼び出しと同じく、この呼び出しはクロックを読む可能性があるものとして
  扱うため、`tick` のたびにクロックをアクティブレベルへ動かして戻します。tick は
  サンプルの呼び出しに加えて、この分のコストも払います。`always_comb` や
  `always @*` からの
  `sample()` は手続きの評価ごとにサンプルし、デザインは変化が届く部分だけでなく
  組み合わせロジック全体を settle のたびに評価します。以前のリリースは
  `cg c = new;` を無視していたため、そのデザインは `Simulator` で構築でき、
  状態にハンドルを持ちませんでした。covergroup はヒープを持たないブラウザ
  （WASI）ビルドでは使えません。
- **ホストクロック。** `Simulation::add_clock` と `Simulation::schedule` は
  イベント信号（エッジ駆動 `always` のクロックや非同期リセット、またはプロセスが待つ
  トップモジュールの1ビット入力）を動かします。プロセスが待つのは、イベント制御が名指す入力
  （`@(posedge clk)`、`@(clk)`）または `wait` 条件が読む入力で、トップモジュール自身、階層名、
  または信号全体をそのままコピーするポート接続・継続代入経由のものに限られます。
  それ以外の信号（データとしてのみ読まれる入力、`iff` 条件、複数ビット入力、`@(posedge (clk & en))` の
  ようなイベント式の被演算子など）に対しては、`schedule` は `NotAnEvent` を返し、`add_clock` は何もしません。
  これらは実行の間に `modify` で駆動します。複数トップモジュール時は `named_events()` が
  各トップからイベント名を返し（`hdl_top.clk`）、`add_clock_by_id` / `schedule_by_id` で駆動できます。
  イベント信号はホストから force できません（`force_signals` は待機される入力もクロックと同様に拒否します）。
  別イベントの計算元となる待機入力上のクロック（ゲーテッドクロック、反転リセットなど）はスケジューラの
  タイムスロットにコンパイルされないため、各エッジがホストの1ステップになります。
- **終了。** `final` 手続きは `$finish`・`$fatal`・明示的な
  `Simulation::finish()` で1度実行します。空の `step()` や `run_until()` の
  終端では、再利用可能な Rust シミュレーションは終了しません。その後は
  スケジュール済みイベントは実行しないため、`final` 内で直接行った
  ノンブロッキング代入のキュー更新は適用されず、`final` 内で宣言した切り離された
  `fork … join_none` の子も初期化されますが開始しません。`initial` 内の `$stop` は
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
- **トップレベルモジュール。** シミュレーションは指定されたトップモジュールを実行します。
  テストベンチの `hdl_top` と `hvl_top` のような複数トップモジュール（23.3.1）を持つデザインは、
  Rust の `SimulatorBuilder::tops` または複数回の `celox run --top` で指定すると、
  `$root` 配下にそれぞれの名前のインスタンス（ポート未接続）として同時に実行します。
  階層名、`%m`、波形スコープ、VPI/リフレクション階層は各トップから始まり（`hdl_top.dut`、`$root.hdl_top.dut`）、
  各トップが `vpi_iterate(vpiModule, NULL)` で返るルートスコープになります。
  信号・force 対象・live 信号も各トップから指定します（`signal("hdl_top.clk")` や
  `child_signal(&[("hdl_top", 0)], "clk")`）。`PreserveTopPorts` は各トップのポートを保持し、
  `named_signals()` は各トップからの信号名（`hdl_top.clk`）、`named_hierarchies()` は各トップの階層、
  `named_hierarchy()` はトップ群を子とする `$root` を返します。複数トップは SystemVerilog ソースのみに対応し、
  パラメータ上書きは取れません。
- **観測できる名前。** interface は使用モジュールへ展開します。interface
  インスタンス `h` のメンバ `m` は信号 `h$m` となり、トップの interface ポートは
  `bus$data` のようなポートとなり、汎用 `interface` ポートを持つモジュールは
  接続 interface ごとに `M$I` として複製します。インスタンス配列要素は宣言添字で
  指定します（`dut.u[3]`、`child_signal(&[("u", 3)], "y")`）。package 変数は
  `p::v`、コンパイル単位変数は `$unit::v` と `sim.signal(…)` で名指します。
  階層名は SystemVerilog コードとホスト API の双方で、`string` 変数を含む信号を指定できます。
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
- **メモリファイル。** <a id="memory-files"></a> `$readmemh` と `$readmemb` は
  実行された時にファイルを読み（21.4）、デザインのコンパイル時には読みません。そのため
  コンパイル済みコード、JIT キャッシュ、ネイティブイメージもその時点のファイルを読みます。
  待機もデザイン状態の読み出しもその前に行わない `initial` ブロックの読み込みは、
  `Simulator` と `Simulation` のどちらでもインスタンスを作るたびに実行します。それ以外の
  読み込みは文の位置で実行し、`always_ff` では活性化のたびに、`always_comb` では
  ブロックが実行されるたびに読みます。相対パスはシミュレーションを実行するプロセスの
  作業ディレクトリから探します。ワード数不一致の警告と読み込みのエラーは
  `RuntimeEvent::Report` になります。エラーはそれ以前に読んだワードを残して読み込みを
  打ち切り、シミュレーションは続行します。読み込んだワードをその後で読む `initial`
  ブロックはプロセスとして実行します。`always_comb` のラッチ推論の検査では、
  読み込みは読み込み得るワードをすべて代入したものとみなします。
  連想配列は、インデックスが 64 ビット以下の整数値で要素が 64 ビット以下の packed 値の
  ときに読み込みます（21.4.1）。常に文の位置で実行し、各ワードはそのアドレスの要素を
  作成または置き換えます。アドレスはインデックスのビットパターンです（Verilator と同じく
  `@fffffffe` は `int` のインデックス -2）。呼び出しにアドレスが無い場合、ファイルの
  アドレスは任意のインデックスを指せます。列挙型のインデックスまたは要素のメンバーでない
  アドレスやワードはエラーです。`banks[i++]` のように配列を選択するインデックスは副作用を
  持ってはいけません。動的配列とキューには読み込みません。

## Veryl と SystemVerilog の混在 {#mixed-language}

言語混在は標準ではなく Celox の機能です。
`Simulator::from_mixed_sources`
（または `Simulation::from_sources(veryl).with_sv_sources(sv)`）が両ソースを
組み合わせます。Veryl は `$sv::Module` で SV モジュールをインスタンス化します。
SV は Veryl モジュールや interface を出力名で参照します
（`prj_Module`、`omit_project_prefix` 時は `Module`、`Veryl.toml` 無しでは
プロジェクト名は `prj`）。

| 観点 | 境界越しの対応 | 境界越しの未対応 |
| --- | --- | --- |
| **モジュールとポート** | どちらの言語もトップに設定可能。双方向の階層またぎ。モジュールパラメータと通常の信号ポート | — |
| **interface と関数** | 双方向に境界を跨ぐ interface および `input` / `output` modport | 言語を跨ぐ interface 関数呼び出し。関数の `import` / `export` modport。契約で表せない文本体を持つ interface ヘッダ関数（ループなど）、ヘッダ依存の仮引数・戻り値幅、対象サイズが interface パラメータに依存する宣言キャスト、記号的に下げられない定数関数 *（これらは単一言語内では利用可能）* |

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
`unsafe` です。`allow_unlinked_dpi_for_elaboration()` は、実行しない elaboration のために
（他のツールと同様に C コードをリンクしない sv-tests ランナーの elaboration モード）、
未解決の import があっても設計を構築します。そうした import の呼び出しは、import 名と
C 本体なしで elaboration されたことを示す実行時エラーになります。
明示的に要求しない限り無効で、シミュレーションや他の構築ではエラーのままです。

| 観点 | 対応 | 未対応 |
| --- | --- | --- |
| **宣言と呼び出し位置** | モジュール・package・`$unit` スコープで宣言した `import "DPI-C"` 関数（`context` import を含む、35.5.3）。`initial`、`always`（`always @*` を含む）、`always_ff`、およびそれらが呼ぶサブルーチンからの呼び出し | task。継続代入および `always_comb` からの呼び出し。`context` import でのスコープ API（`svGetScope`、`svSetScope`、`svGetNameFromScope`、`svGetScopeFromName`、ユーザーデータ）や export 済み SV サブルーチンの呼び出し（対応 C シンボルを提供していないため、それらを使うライブラリは未解決シンボルエラーで読み込み失敗） |
| **引数と戻り値型** | 最大16個の `input` / `output` / `inout` 引数。`bit`、`logic`（`svBit` / `svLogic`）、`byte`、`shortint`、`int`、`longint`（符号有無可）、`chandle`、`string`（`const char *`）、`bit`/`logic`/`reg` の packed 配列、`integer`、`time`（`svBitVecVal` / `svLogicVecVal` のワード列）。結果型は packed 配列より前の各型と `void`（35.5.5） | 17個以上の引数。`real`、`shortreal`、名前付きの型（`typedef`・列挙型・構造体）、unpacked 配列および open array |
| **`export "DPI-C"`（35.7）** | 宣言の受理と検証 | C コードからのエクスポート済み SV 関数呼び出し（上記 `context` import のスコープ API 制約参照） |

`output`・`inout` 引数はその C 型へのポインタ（`string` では `const char *` へのポインタ）です。
関数に渡した string と packed 配列は関数が戻るまで有効で、関数が返す、または `output` に書く string は戻った時点でコピーします。
2値シミュレーションでは、関数が `logic` 引数に書いた不定ビットは 0 として読みます。
import を呼ぶ `always @*` ブロックは読み取り対象の変化時にのみ実行し（9.4.2.2）、
SystemVerilog 側の引数が変わらなければ C 関数の結果が変わっても再呼び出しされません。

UVM の C ライブラリ関数は Celox が提供し、同名のユーザー実装を覆い隠します。
`uvm_re_*` は Rust の `regex` クレートを使うため、まれな構文で POSIX 拡張正規表現と異なります。
`uvm_dpi_get_next_arg_c` は `celox` と plusargs を返し、`uvm_dpi_get_tool_name_c` は `Celox` を返します。
`uvm_hdl_*` と `uvm_polling_*` は 0 または `null` を返すため、UVM はそれらの機能を利用不可と報告します。
UVM 1.2 の `uvm_re_match`・`uvm_glob_to_re`・`uvm_dump_re_cache`・`uvm_dpi_regcomp`・`uvm_dpi_regexec`・`uvm_dpi_regfree` も提供します。
これらのレポート（`UVM/DPI/REGEX_INV`・`UVM/DPI/REGEX_MAX`・`UVM/DPI/REGCOMP` とキャッシュメッセージ）は
UVM の C ライブラリと同じく UVM パッケージがエクスポートする `m__uvm_report_dpi` を通して出すため UVM のレポートとして数えられ、
`invalid glob or regular expression` の後の理由は `regerror` ではなく `regex` クレートのものです。

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
