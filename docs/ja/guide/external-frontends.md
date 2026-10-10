# 外部フロントエンド連携

このガイドは frontend crate/package の実装者向けです。アプリケーションには
frontend 固有の artifact 型と `from_my_artifact` のような constructor を
提供してください。`celox_frontend_sdk::FrontendArtifact` は frontend から
Celox へ渡すための bridge であり、frontend の公開 artifact model の代わり
ではありません。

## frontend 内で Celox へ lower する

frontend adapter から `celox-frontend-sdk` を使い、独自の elaboration 結果を
検証済みの Celox module へ変換します。

```rust
use celox_frontend_sdk::{BinaryOp, FrontendArtifact, ModuleBuilder, ValueType};

fn lower_to_celox(artifact: &MyArtifact) -> Result<FrontendArtifact, MyError> {
    let byte = ValueType::bits(artifact.width)?;
    let mut module = ModuleBuilder::new(&artifact.module_name)?;
    let a = module.input("a", byte)?;
    let b = module.input("b", byte)?;
    let y = module.output("y", byte)?;
    let a_expr = module.read(a)?;
    let b_expr = module.read(b)?;
    let sum = module.binary(BinaryOp::Add, a_expr, b_expr, byte)?;
    let y_target = module.whole(y)?;
    module.assign(y_target, sum)?;
    Ok(module.finish())
}
```

`MyArtifact` と `MyError` は frontend 側の型です。SDK の validation error は
frontend の error 型へ変換し、通常のアプリケーション API に Celox の JSON
を露出させないでください。

## frontend 固有の Rust constructor を公開する

frontend crate は extension trait を使い、re-export した simulator に
artifact 固有の associated function を追加できます。

```rust
pub use celox::Simulator;

pub trait MyFrontendSimulatorExt: Sized {
    fn from_my_artifact(artifact: MyArtifact) -> Result<Self, MyError>;
}

impl MyFrontendSimulatorExt for Simulator {
    fn from_my_artifact(artifact: MyArtifact) -> Result<Self, MyError> {
        let artifact = lower_to_celox(&artifact)?;
        Ok(Simulator::from_frontend(artifact).build()?)
    }
}
```

アプリケーションは `FrontendArtifact` を意識せず、frontend の API だけを
使用します。

```rust
use my_frontend::{MyFrontendSimulatorExt as _, Simulator};

fn main() -> Result<(), my_frontend::Error> {
    let artifact = my_frontend::load("design.myhdl")?;
    let mut sim = Simulator::from_my_artifact(artifact)?;

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 23u8);
    })?;
    assert_eq!(sim.get(y), 33u8.into());
    Ok(())
}
```

```bash
cargo build --release
```

Rust binary adapter 内の `celox` と `celox-frontend-sdk` は同じ version を
使用してください。JavaScript addon は同じ version の `celox-napi` も使用
します。そのほかの `celox-*` crate は実装依存です。

## TypeScript package から `fromMyArtifact` を公開する

N-API または WASI addon を同梱する frontend は、その addon で
`MyArtifact` を受け取り、Celox 標準の raw simulator handle を返せます。
`celox-napi` は native build と `wasm32` build の両方で使える Rust 専用の
adapter constructor を提供します。

```rust
use celox_napi::NativeSimulatorHandle;
use napi_derive::napi;

#[napi]
pub fn from_my_artifact(artifact: MyArtifact) -> napi::Result<NativeSimulatorHandle> {
    let artifact = lower_to_celox(&artifact)
        .map_err(|error| napi::Error::from_reason(error.to_string()))?;
    NativeSimulatorHandle::from_frontend(artifact, None)
}
```

frontend の TypeScript entry point では、この handle を通常の Celox runtime
でwrapし、公開名は frontend 固有のままにします。

```ts
import { Simulator, type FrontendSimulatorHandle } from "@celox-sim/celox";
import { fromMyArtifact as nativeFromMyArtifact } from "./native.js";

export function fromMyArtifact<P>(artifact: MyArtifact): Simulator<P> {
  const handle: FrontendSimulatorHandle = nativeFromMyArtifact(artifact);
  return Simulator.fromFrontendHandle<P>(handle);
}
```

この経路は `MyArtifact -> frontend の Rust lowering -> FrontendArtifact` を
in-memory の Rust value として渡します。`fromFrontendArtifact` は呼ばず、
Celox artifact JSON も parse しません。build 可能な addon と load test は
`examples/my-frontend-napi` にあります。

## process と delay

`initial` block のような手続き的な process は `ModuleBuilder::process` で
追加できます。process は時刻 0 に開始し、文を順に実行します。

| 文 | 動作 |
|---|---|
| `Assign` | blocking 代入。後続の文は新しい値を読み、他の logic は process が suspend した後に新しい値を見ます。 |
| `If` | 条件で分岐します。不定 bit を含む条件は偽として扱います。 |
| `While` / `Forever` | 条件が成り立つ間、または永久に body を繰り返します。 |
| `Delay` | 指定した時間単位だけ suspend します。量は 64 bit 以下です。 |
| `ClockCycles` | clock の立ち上がりエッジが指定回数過ぎるまで suspend します。clock は register の clock で、`ModuleBuilder::clock_period` で周期を設定しておきます。runtime は最初の待ちからその clock のエッジを生成し、process は最後に数えたエッジの 1 周期後、次のエッジの前に再開します。 |
| `Finish` | simulation を終了します。 |

```rust
// forever #5 clk = ~clk;
let half_period = module.constant(Constant::two_state(5u8, 8)?);
let clk_expr = module.read(clk)?;
let toggled = module.unary(UnaryOp::BitNot, clk_expr, bit)?;
module.process(vec![Statement::Forever {
    body: vec![
        Statement::Delay { amount: half_period },
        Statement::Assign { target: module.whole(clk)?, value: toggled },
    ],
}])?;
```

process は時刻付きの `Simulation` でだけ実行されます。各 process は再開可能な
kernel に compile され、delay が満了すると simulation の scheduler が再開
します。同じ時刻に再開する process は宣言順に実行されます。process が見るのは前の時刻で
settle した state です。process が起こした clock や reset の edge は、
schedule された event と同様にその時刻の register を trigger します。0 の delay
を置くと、process は同じ時刻のうちに、他の process が実行され、その edge が
trigger した register が settle した後で再開します。そのため 0 の delay で区切った
pulse も edge になります。

`ClockCycles` で待つ clock は、いずれかの process が待っている間、半周期ごとに
toggle します。生きている process がすべて同じ clock を待っていて他に schedule
された event がないときは、その edge は backend が生成したコードの中で fused tick
として実行されるため、長い待ちも event 駆動の `Simulator` で同じ回数 tick する
のと同程度の cost で済みます。この間は clock signal 自身の edge と外部 component
の edge ごとの hook を省略し、他の process や event が割り込むと再び有効になります。

process が書けるのは、continuous assignment や register が駆動していない
output と internal signal です。同じ signal を複数の process が書いても
構いません。checkpoint は suspend 中の process を保存しますが、process を
持つ simulation の state file にはまだ対応していません。process を持つ
artifact は Veryl native testbench から instantiate できません。

## artifact format の制限

format version 1 が受け取るのは平坦化済みの1 module です。型付き signal、
constant、組み合わせ式と代入、edge-triggered register、非同期 reset、同期
enable、初期値を扱えます。format version 2 で process が加わりました。
process を持たない artifact は builder が version 1 として書き出すので、
version 1 しか読まない consumer もそのまま受け取れます。hierarchy、memory、
latch、custom primitive、bidirectional signal は SDK builder を呼ぶ前に
frontend 側で lower してください。

Celox は compile 前に artifact を再検証します。artifact は SDK builder で
生成し、Rust の値を直接 `Simulator::from_frontend` に渡してください。
