# celox-frontend-sdk

`celox-frontend-sdk` is the stable authoring boundary for external Celox
frontends. It models one flattened, elaborated netlist without depending on
Celox scheduling, SIR, optimization, or backend crates.

```rust
use celox_frontend_sdk::{BinaryOp, ModuleBuilder, ValueType};

let byte = ValueType::bits(8)?;
let mut module = ModuleBuilder::new("Adder")?;
let a = module.input("a", byte)?;
let b = module.input("b", byte)?;
let y = module.output("y", byte)?;
let a = module.read(a)?;
let b = module.read(b)?;
let sum = module.binary(BinaryOp::Add, a, b, byte)?;
let y = module.whole(y)?;
module.assign(y, sum)?;
let artifact = module.finish();
# Ok::<(), celox_frontend_sdk::BuildError>(())
```

`FrontendArtifact` is an adapter boundary, not the artifact type an application
should consume. A frontend crate should keep the conversion internal and expose
an API named for its own representation:

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

After importing the extension trait, applications call
`Simulator::from_my_artifact` and do not need to know that the adapter uses
Celox's bridge type.

Pass the resulting Rust value directly to Celox. The JSON representation is a
separate transport format and is not required to build a Rust simulator binary.

Artifact format version 1 models one flattened module and does not support
bidirectional (`inout`) signals. External frontends must lower them to separate
input, output, and output-enable signals before building the artifact.

Frontend adapters that use both `celox-frontend-sdk` and `celox` must keep their
versions aligned.
