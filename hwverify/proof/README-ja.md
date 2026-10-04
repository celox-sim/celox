# メモリ簡約の正しさ

対象は `crates/hwverify-ir/src/term.rs` の `memory_read` / `memory_write`。Lean 4.34.1 + Std のみを使用する。

## 証明したもの

`MemoryRules.lean` は値・メモリの相互再帰AST、意味論、および実行可能なsmart constructorを定義する。

- `memoryRead_sound`: 任意のメモリ式・アドレス・自然数budgetで、簡約結果の評価値が元のreadと一致
- `memoryWrite_sound`: 任意のメモリ式・アドレス・書込値で、簡約結果のメモリ全体が元のwriteと一致

Rustの4規則との対応:

| Rust規則 | Leanモデル |
|---|---|
| read_after_same_write | store先とread先の構文が同じなら最新値 |
| read_after_write_alias_split | 構文が違っても実アドレスは一致し得るため、解釈値の等号で分岐 |
| constant_memory_read | budget=0を含め、定数メモリの値を返す |
| last_write_wins | 同じ構文のアドレスへの直前のstoreを1個だけ除去 |

アドレス構文Aと実アドレスDを分け、任意の解釈関数 A → D を使う。解釈関数の単射性は仮定しない。アドレス幅・メモリ容量・値型には特定の制限を置かない。RustのBitVec/SMT配列はこのモデルの具体化として読める。

budgetを使い切ったreadは元のreadとして残す。定数メモリの規則はbudgetに依存しない。全木を一度に書き換えるコンパイラの証明ではなく、Rustが実際に使う2つのsmart constructorの意味保存を証明している。

## どこまで保証するか

Lean kernelが検査するのはこの型付きモデル。Rustプログラムからの自動抽出ではなく、Rust実装との対応はまだ形式証明していない。

Rustは文字列op・Vec引数・実行時Sortを持つ。Leanの型付きASTに対応するwell-formedな入力であること、構文等価判定の健全性、frontendの型検査、Rc/Vec実装、SMT出力、証明義務生成、Z3は今回の定理の対象外。特にRustの`starts_with("(as const ")`の判定はfrontendが作る正しいノードを想定している。この証明だけで処理系全体が検証済みとは言わない。

`#print axioms` の結果は2つの健全性定理とも `propext, Quot.sound`。sorry、新規公理、native_decideは使っていない。

## 実装との照合と負例

`check_rust_rules.rs` は実物の `../../crates/hwverify-ir/src/term.rs` を直接importし、Z3を使わない独立評価器と比較する。

- 2-bitアドレス・2-bitワードの全256メモリ
- アドレス変数3個の全64代入。同じ構文・異なる構文での実アドレス衝突を含む
- 2回の書込値の全16組、2種類の書込先構文、3種類のread先、budget 0..3
- read比較6,291,456件、write比較524,288件
- 定数メモリread、4規則すべての実行も確認

これは有限範囲の差分テストであり、Rustの全入力に対する証明ではない。

`recheck.py` はLeanとRustそれぞれについて「異なる構文なら非aliasと決めつける」「最初のwriteを残す」という誤変更を一時コピーに施し、証明失敗・具体的な値不一致で拒否されることも確かめる。元ソースは変更しない。失敗終了だけでなく、Lean診断／Rustのsemantic mismatchを確認する。

## 再実行

Lean4.34.1とRustをPATHに用意して:

```
python3 proof/recheck.py --lean lean --rustc rustc
```

各コマンド120秒上限。`proof/results.json` に実行時間・診断・対象ソースSHA256を記録する。配布用の機械依存パスを書き換えて証明を成立させる必要はない。

今回、Leanの基礎的な場合分けとbudget帰納法だけで通った。単一回のLean検査は約0.5秒だった（正確な実測はresults.json）。一般的な自動探索に頼らなくても、この小さいメモリ規則群の根拠は保持できる。一方、Rustからこのモデルへの対応は次に残る課題であり、テストで埋めた部分を証明済みと混同しない。


## プログラム契約の推論規則

`ProgramRules.lean` には `total_correctness` を追加した。初期不変条件・active時の保存・自然数rankの厳密減少・terminal時のpostを仮定すると、rank(initial)以下のステップでterminalかつpostへ到達する。unsigned BitVecの値は自然数へ解釈できるため、checkerが生成する義務の数学的な根拠になる。`lift_invariant` は仕様1ステップまたはstutterに対応する実装ステップへ不変条件を運ぶ。

これらの定理を具体的CPUやRustの義務生成器へ形式的にinstantiateしたわけではない。Z3結果をLeanが読み込んで検査する仕組みでもない。実装の進行条件との合成・外部stall/resetの環境条件は、[program契約](../docs/program-contracts.md)で説明する信頼境界として残る。

`total_correctness`の依存は標準のpropext/Classical.choice/Quot.sound、`lift_invariant`はpropextのみ。`recheck.py`で両ファイルの検査を再実行する。
