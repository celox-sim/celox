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

In Rust, pass the result to `celox::Simulator::from_frontend`. For TypeScript
testbenches, serialize it with `FrontendArtifact::to_json` and call
`Simulator.fromFrontendArtifact` or `Simulation.fromFrontendArtifact` from
`@celox-sim/celox`.

A Veryl native testbench can instantiate the artifact module using its existing
`$sv::ModuleName` external-module syntax. Pass the artifact and testbench
sources to `celox::Simulator::from_frontend_with_testbench`; the TypeScript
equivalent is `runTestWithFrontendArtifact`.

The versioned JSON representation is checked at decode time. Consumers should
produce it through `to_json` instead of relying on its field spelling as a
hand-authored format.

Artifact format version 1 models one flattened module and does not support
bidirectional (`inout`) signals. External frontends must lower them to separate
input, output, and output-enable signals before building the artifact.
