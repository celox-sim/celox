# celox

`celox` is the Rust host API for the Celox RTL simulator. External frontend
crates use its low-level `Simulator::from_frontend` hook after lowering their
own artifact through `celox-frontend-sdk`.

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

Celox is experimental and its Rust API is not yet covered by a 1.0 stability
guarantee.
