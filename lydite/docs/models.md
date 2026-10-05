# CPU example models

These abstract custom-ISA fixtures are distinct from the imported [selected RV32I implementation](rv32i.md). Historical timings and run logs remain in `audit/` and `results/`; fresh validation commands are in the [audit index](../audit/README.md).

## 今回の対象

`examples/pipeline.lyd` / `pipeline.json` は実際に最大3命令を同時に保持する
in-order CPUの宣言的モデル。従来のfetch→executeを1命令ずつ交互に動かす例とは別。

- D: fetch済み命令。レジスタまたはforwarding経由でoperandを読み、Xへ送る
- X: 演算。ALU結果はDへforwardできる。LOAD結果はWへの登録までDへ渡せない
- W: 命令順にretire。ここだけがarchitectural registerとretirement PCを更新する
- 同一cycleでW retire、X→W、D→X、次命令fetchが可能
- DがXのLOAD結果を使う場合、D/fetchを保持しXへbubbleを挿入。XのLOADはWへ進む
- X/W両方がDのsource registerへ書く場合、若いXを優先。次cycleのload-useはWからforward
- 外部stallは全実装stateを保持。同期resetがstallより優先し、全stageを空にする

データ幅4bit、レジスタ2本、PC2bit、4命令の循環ROM、readonly data4word。
ROMとdataは**任意の入力**をreset時にcaptureし、その後は不変。特定のprogramだけを
仮定しない。PC・加算はwrapする。分岐、HALT、STORE、例外、cache、予測、OoOは含まない。
実行長は4命令で止めず、PCがwrapしながら任意回数実行する。

命令8bit: `op[7:6] rd[5] rs[4] imm[3:0]`。

| op | 命令 | 意味 |
|---|---|---|
| 0 | MOVI | r[rd] = imm |
| 1 | ADDI | r[rd] = r[rs] + imm (mod16) |
| 2 | XORI | r[rd] = r[rs] xor imm |
| 3 | LOAD | r[rd] = data[imm & 3] |

## 普遍的な検証の意味

ISAは1命令を原子的に実行する。実装のW retire時だけISAを1step進め、それ以外はstutter。
次の関係をresetから帰納的に保存する。

1. ISAと実装のarchitectural register、retirement PC、ROM/dataが一致
2. validなWは現在のretirement PCの命令で、W.resultは現在ISA状態でのその命令の結果
3. validなXはWの後、validなDはXの後という順番。各IRは対応するROM[PC]と一致
4. Xがsourceを使う場合、operandは現在registerにpending Wの書込みを適用した値
5. fetch PCはretirement PC + valid stage数 (mod4)

invalidなstageのpayloadには意味を与えない。現在入力、具体実行観測、到達状態の推定を
bindingへ加えない。ISAとbindingを弱めて悪い実装を通すこともしない。

8義務はbinding非空、reset、microstep refinement、commit eligibility、stall保持、
progress非空、commit非空、非commit時rank減少。rankはW/X/Dの一番古いvalid stageから
0/1/2/3。stallしない非commit cycleでは必ず減る。したがって外部stallが解除され続ける
条件下で次のretireへ進む。無限stallがなくなることやprogramの停止を証明したものではない。

この検証は有限サイズの状態空間に対する**1step帰納証明**であり、有限cycleまで展開した
BMCやsimulationのみの成功ではない。一方、仕様・bindingの適切さや実際のRTLとの一致は
別の問題であり、この例もRTL import / synthesisではない。

## 対象と命令

`examples/build_branch_pipeline.py` は従来の `build_pipeline.py` と別のgenerator。
`branch_pipeline_w{4,8,16,32}.{json,lyd}` は同じD/X/W制御構造を持ち、データ幅だけを変える。
既存のpipeline例・過去の監査結果は変更しない。RTL importやout-of-order CPUのモデルではない。

全幅で2レジスタ、2bit PC、4命令ROM、readonly data 4word。ROM/dataは任意の入力をreset時に
captureし、その後の入力変化を無視する。特定プログラムだけを仮定しない。
4命令で実行を停止せず、wrap・分岐・自己ループを含む任意回数の実行が対象。

データ幅をWとすると命令幅はW+5bit。
`op[W+4:W+2] rd[W+1] rs[W] imm[W-1:0]`。

| op | 命令 | 意味 |
|---|---|---|
| 0 | MOVI | r[rd] = imm、PC = PC+1 |
| 1 | ADDI | r[rd] = r[rs]+imm (mod 2^W)、PC = PC+1 |
| 2 | XORI | r[rd] = r[rs] xor imm、PC = PC+1 |
| 3 | LOAD | r[rd] = data[imm & 3]、PC = PC+1 |
| 4 | BZ | r[rs]=0ならPC=imm & 3、それ以外PC=PC+1。レジスタ書込みなし |
| 5–7 | NOP | レジスタ書込みなし、PC = PC+1 |

PCはmod4でwrap。BZのtargetは相対offsetではなく絶対2bit addressで、rdと即値の上位bitは
動作に影響しない。非分岐命令の意味は旧pipelineと同じだが、命令encodingは異なる。
STORE、mutable memory、例外、HALT、cache、予測器学習、OoOは対象外。

## X解決とflushのcycle semantics

- fetchでは常にPC+1へ進むalways-not-taken予測を使う
- Dはレジスタ、XのALU結果、Wの結果からsourceを選ぶ。若いXがWより優先
- BZもsourceを消費する。直前LOADに依存するとD/fetchを保持し、Xへbubbleを挿入。
  LOADがWへ進んだ次cycleにはWからforwardできる
- branch/NOPは書込み命令ではないため、X/Wのforward候補にもRAW producerにもならない
- validなXのBZがtakenなら、現在Dと同cycleの順次fetchを捨てる。
  次X/Dはinvalid、fetch PCはtargetへredirectする。payloadをゼロにする必要はない
- 同じcycleでも古いWは通常どおりretireする。解決したXの分岐自身はWへ進む
- Wには`w_next_pc`を保持し、分岐のretireで初めてarchitectural retirement PCを更新する。
  fetch PCのredirect自体はarchitectural effectではない
- not-takenでは通常のD→X、fetch→Dを継続する
- 外部stallではredirect/flushを含む全実装stateを保持。同期resetがstallより優先し、
  全stageをinvalidにしてPC/registerを0へ戻し、ROM/dataを取り直す

Xの命令は同時にLOADとBZにはならないため、taken branchと同じXに起因するload-use hazardの
同時成立はない。確認すべき相互作用はLOAD→BZの待機と、分岐解決時の外部stall/resetである。

## 帰納関係と主張

ISAは1命令を原子的に実行し、実装のW commit時だけ1step進める。それ以外はstutter。
現在stateだけで次のbindingを記述する。現在入力やsimulation観測を仮定へ入れない。

1. architectural register、retirement PC、capture済みROM/dataがISAと実装で一致
2. validなWは現在PCのROM命令。writerならW.resultがISAの結果と一致し、
   全命令でW.next_pcがISAの次PCと一致
3. validなXはWの解決済みnext_pc（Wがなければ現在PC）の命令。
   sourceを使うXのoperandは現在registerにpending Wの有効な書込みを適用した値
4. DとfetchはXの後の予測上の順次位置。Xがtakenでもまだ未解決なのでDのwrong-path命令を許す。
   次stateではXの解決に従いそれをkillする
5. invalid stageのpayloadと非writerのW.resultには意味を与えない

これによりwrong-path命令がDに存在してもarchitectural registerへの書込みには至らず、
古いWの正当な書込みと分岐自身のretireを失わないことを1step保存で検査する。
architectural effectはWのvalid/write guardだけに集約している。

従来と同じ8義務（binding非空、reset、microstep refinement、commit eligibility、stall保持、
progress非空、commit非空、非commit時rank減少）を使用。rankはW/X/Dの最古validに従い0/1/2/3。
非stall・非commitなら次rankが厳密に減るので、外部stallが解除され続ける条件下で次retireへ進む。
program停止、branch targetへの到達、無限stallの解消は主張しない。
非空性SATは関係の非空性であり、SATで得た任意stateのreset到達性証明ではない。

これは有限bit幅の状態空間に対する時間無制限の1step帰納refinement。
有限cycleのsimulation/BMCだけを成功させたものではない。
有限solverのUNSATには独立certificateがなく、Rust bit-blast/CDCL、構造的kernel、loweringと
bindingの適切さを信頼する。既存Leanのメモリ定理が新モデルやsolverを証明したとは主張しない。
