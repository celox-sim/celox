# External Frontend Integration

This guide is for authors of frontend crates and packages. Applications should
work with the frontend's own artifact type and constructor, such as
`from_my_artifact`. `celox_frontend_sdk::FrontendArtifact` is the bridge from
that frontend into Celox; it should not replace the frontend's public artifact
model.

## Lower into Celox inside the frontend

Use `celox-frontend-sdk` in the frontend adapter. The adapter translates its
own elaborated representation into a validated Celox module:

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

`MyArtifact` and `MyError` belong to the frontend. Convert SDK validation
failures into the frontend's error type instead of exposing a Celox JSON blob as
the normal application API.

## Expose a frontend-specific Rust constructor

The frontend crate can add an artifact-specific associated function to the
re-exported simulator with an extension trait:

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

An application then depends on the frontend crate and builds an ordinary Rust
binary without knowing about `FrontendArtifact`:

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

The `celox` and `celox-frontend-sdk` dependencies in a Rust binary adapter must
use matching versions. A JavaScript addon also uses the matching `celox-napi`
version. Other published `celox-*` crates are implementation dependencies.

## Expose `fromMyArtifact` from a TypeScript package

A frontend that ships an N-API or WASI addon can accept `MyArtifact` in that
addon and return Celox's standard raw simulator handle. `celox-napi` provides
the Rust-only adapter constructor for both native and `wasm32` builds:

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

The frontend's TypeScript entry point wraps that handle in the normal Celox
runtime while retaining the frontend-specific public name:

```ts
import { Simulator, type FrontendSimulatorHandle } from "@celox-sim/celox";
import { fromMyArtifact as nativeFromMyArtifact } from "./native.js";

export function fromMyArtifact<P>(artifact: MyArtifact): Simulator<P> {
  const handle: FrontendSimulatorHandle = nativeFromMyArtifact(artifact);
  return Simulator.fromFrontendHandle<P>(handle);
}
```

This path is `MyArtifact -> frontend Rust lowering -> FrontendArtifact` as an
in-memory Rust value. It does not call `fromFrontendArtifact` and does not parse
Celox artifact JSON. A buildable addon and load test live in
`examples/my-frontend-napi`.

## Artifact format limits

Format version 1 accepts one flattened module. It supports typed signals,
constants, combinational expressions and assignments, edge-triggered registers,
asynchronous reset, synchronous enable, and initial values. A frontend must
lower hierarchy, memories, latches, custom primitives, and bidirectional signals
before calling the SDK builder.

Celox validates the artifact again before compilation. Produce it through the
SDK builder and pass the Rust value directly to `Simulator::from_frontend`.
