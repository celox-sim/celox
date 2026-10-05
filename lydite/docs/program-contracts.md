> このページの実測は0.3の記録。0.4では手動splitを省略した自動選択を追加した。
> 新しい実測・残る制限は [proof search and partitioning](automatic-proofs.md)。

# ISA配列和からfetch/execute実装へ

## 結果の範囲

固定16要素の任意8-bit配列の和（mod 256）について、初期条件・帰納的不変条件・
停止時postcondition・有限rankをRust+Z3で検査する例を追加した。同じJSONのCPU
状態対応検査も通すため、ISAレベルの結果をfetch/execute実装に接続できる。

これはbounded executionで33命令を展開する検査ではない。任意の不変条件内状態
について1命令分の保存とrank減少を証明する。ただし固定16要素の仕様prefix和は
有限式として展開している。可変長配列や無限幅整数一般の証明ではない。

## プログラムとISAの拡張

旧`cpu.json`は変更しない。新しい`array_sum.json`だけに8-bit indexを追加し、
予約NOPだったopcode6/7を次の意味にする。

- ADDX: ACCにdata[idx]をmod256で加える
- IXJ addr: idxをmod256で1増やし、新idxが非zeroならaddrへ分岐

resetインターフェイスにdata入力とstart_index入力を追加。preconditionは、
start_index=240、データは任意配列p.data、programは次の3命令、とする。

```
0: ADDX
1: IXJ 0
2: HALT
```

アドレス240..255の16要素を読み、33 ISA命令でHALT。データメモリは変更しない。
ISA/実装のデータパス式を共有する既存の構成は継続しており、独立に書かれたRTLの
検証ではない。独立整数ISAオラクルとの具体比較は別に行う。

## 契約

p.dataは全実行を通じて固定のghost parameter。live入力ではない。

- PC=0: 240≤idx≤255、ACC=先頭からidxの直前までのprefix和
- PC=1: 同じindex範囲、ACC=prefix和+data[idx]
- PC=2: idx=0、ACC=16要素全和、未HALT
- PC=3: idx=0、ACC=16要素全和、HALT済み
- 全状態でprogram固定、mem=p.data

rankは10-bit unsigned。PC=0で2*(256-idx)+1、PC=1で2*(256-idx)、
PC=2で1、HALT済みで0。preconditionで初期rank=33となり、各ISA命令で減る。
実装のbindingがstutter時のISA状態を保持し、非commit rankがfetchからexecuteへの
進行を検査する。reset後に再resetせずstall=Falseなら、66 microcyclesで終了する。
停止し続ける外部stallや、繰り返しreset下の終了は主張しない。

## 再利用可能な義務

`src/program.rs`が契約を読み、preの非空性、reset入力が実際にassertされた初期化、不変条件保存、step可用性、
rank減少、postcondition、停止後のstep禁止を生成。入力依存invariantは禁止する。
programが不正でも「実装がその不正programを正しく実行する」refinementだけは通る。
このためprogram側の義務を別に要求することが重要。

## 自動化と残る手作業

最初の単一保存義務は10秒制限でUNKNOWNだった。失敗を成功にしない。
再現入力は`examples/array_sum_baseline_unsplit.json`、生証跡は
`results/array_sum_baseline_unsplit/`。動作する版は同じ仕様式のまま、pc0..2 / idx240..255
という2つのcompact split hintsを使った。Rustが値ごとの分割・範囲外other・積を作り、
coverageも検査する。人が各ケースにlemma/simpを記述する必要はない。

ただし不変条件・rank・仕様式とsplit hintsの選択はまだ手作業。完全自動化、
SMT性能問題の根本解決、任意プログラムでのスケーラビリティを示したものではない。
正規化の形式化は`proof/README-ja.md`を参照。Rust/Z3との信頼ギャップは残る。

## 再実行

```
python examples/build_array_sum.py
cargo test --locked
cargo run --locked -- examples/array_sum.json --out results/array_sum --z3 /path/to/z3
python audit/array_sum_simulation.py
```

負例は誤ったloop target、誤indexed-load、誤post、false invariant、false pre、
減少しないrank、input依存invariant、不足case。ソルバ証跡は各test-evidence配下にも
保存する。UNKNOWN/異常出力を成功として扱わない既存テストも維持する。

## 検証の実測と追加の信頼境界

最終テスト一覧は`cargo test --locked`。配列和の成功例は84義務。1回の実測ではwall約1.986秒、report中のquery時間合計約0.986秒。
query時間にはZ3プロセス起動等が含まれるが、Rustの式構築・SMT出力生成は合計外。
同じ契約のsplit無し版は保存義務が10秒timeout。独立した具体ISA比較は
40試行・3310microcycles・1320commitsで通過した。旧CPUの独立監査も
23試行・7300cycles・1161commitsで再通過。

```
python audit/replay_program_counterexamples.py
```

Rust/Z3が返したprogram反例を独立IR評価器で再生する検査も追加。
`audit/array_sum_verification_results.json`が各例のstatus・時間・失敗義務の一覧。

`proof/ProgramRules.lean`は、帰納的不変条件・自然数rankの減少から終了とpostを導く
抽象定理および1-step stuttering liftを扱う。Rustの義務生成との形式的対応、
この具体CPUのbitvector rankとの接続、SMT UNSATの証明証跡検査まで完了した
ものではない。この境界は`proof/README-ja.md`でも明記する。
