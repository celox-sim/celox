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

## artifact format の制限

format version 1 が受け取るのは平坦化済みの1 module です。型付き signal、
constant、組み合わせ式と代入、edge-triggered register、非同期 reset、同期
enable、初期値を扱えます。hierarchy、memory、latch、custom primitive、
bidirectional signal は SDK builder を呼ぶ前に frontend 側で lower して
ください。

Celox は compile 前に artifact を再検証します。artifact は SDK builder で
生成し、Rust の値を直接 `Simulator::from_frontend` に渡してください。
