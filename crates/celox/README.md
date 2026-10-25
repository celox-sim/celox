# celox

`celox` is the Rust host API for the Celox RTL simulator. External frontend
crates use its low-level `Simulator::from_frontend` hook after lowering their
own artifact through `celox-frontend-sdk`.

The default `host-runtime` feature provides native, interpreter and tiered
execution. Optional execution engines are enabled explicitly:

| Feature | Enables |
| --- | --- |
| `cranelift` | `build_cranelift()` and the Cranelift JIT backend |
| `wasm-runtime` | `build_wasm()` using Wasmtime |
| `wasm-component` | Hosting Veryl `.wasm` component libraries |
| `veryl-reference` | Development comparisons against upstream Veryl's simulator |

Native component libraries and WASM byte generation do not need Wasmtime.
Cross-codegen features enable Cranelift for their host execution fallback.

A frontend crate should wrap that hook with a constructor named for its own
artifact:

```rust,ignore
pub use celox::Simulator;

pub trait MyFrontendSimulatorExt: Sized {
    fn from_my_artifact(artifact: MyArtifact) -> Result<Self, MyError>;
}

impl MyFrontendSimulatorExt for Simulator {
    fn from_my_artifact(artifact: MyArtifact) -> Result<Self, MyError> {
        let celox_artifact = lower_to_celox(&artifact)?;
        Ok(Simulator::from_frontend(celox_artifact).build()?)
    }
}
```

Applications then use the frontend API and do not depend on Celox's bridge
artifact model:

```rust,ignore
use my_frontend::{MyFrontendSimulatorExt as _, Simulator};

let artifact = my_frontend::load("design.myhdl")?;
let mut sim = Simulator::from_my_artifact(artifact)?;
```

See the [external frontend guide](https://celox-sim.github.io/celox/guide/external-frontends)
for the complete adapter pattern and artifact format limits.

Celox is under active development and its Rust API is not yet covered by a 1.0
stability guarantee.
