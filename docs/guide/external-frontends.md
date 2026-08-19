# External Frontends

An external frontend can target Celox without depending on compiler-internal
crates. Build a validated module with the published `celox-frontend-sdk` crate,
serialize its versioned `FrontendArtifact`, and pass that JSON to a Celox host.

```rust
use celox_frontend_sdk::{BinaryOp, ModuleBuilder, ValueType};

let byte = ValueType::bits(8)?;
let mut module = ModuleBuilder::new("NetAdder")?;
let a = module.input("a", byte)?;
let b = module.input("b", byte)?;
let y = module.output("y", byte)?;
let a_expr = module.read(a)?;
let b_expr = module.read(b)?;
let sum = module.binary(BinaryOp::Add, a_expr, b_expr, byte)?;
let y_target = module.whole(y)?;
module.assign(y_target, sum)?;

let artifact_json = module.finish().to_json()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The first SDK release represents one flattened module. It supports typed signals,
constants, combinational expressions and assignments, edge-triggered registers,
asynchronous reset, synchronous enable, and initial values. Hierarchical netlists,
memories, latches, and custom primitives should be flattened or lowered by the
external frontend before emitting an artifact. Bidirectional (`inout`) signals
are also unsupported in artifact format version 1; lower them to separate input,
output, and output-enable signals.

## TypeScript testbenches

The artifact retains port and signal metadata, so the regular typed DUT API works:

```ts
import { Simulator } from "@celox-sim/celox";

interface NetAdderPorts {
  a: bigint;
  b: bigint;
  readonly y: bigint;
}

const sim = Simulator.fromFrontendArtifact<NetAdderPorts>(artifactJson);
sim.dut.a = 10n;
sim.dut.b = 23n;
console.assert(sim.dut.y === 33n);
sim.dispose();
```

Use `Simulation.fromFrontendArtifact` when the testbench needs timed clocks and
scheduled events.

## Veryl native testbenches

`runTestWithFrontendArtifact` compiles a Veryl test module and makes the artifact
module available through the existing external-module namespace:

```veryl
#[test(t)]
module NetlistTb {
    var a: logic<8>;
    var b: logic<8>;
    var y: logic<8>;
    inst dut: $sv::NetAdder (a, b, y);

    initial {
        a = 8'd10;
        b = 8'd23;
        $assert(y == 8'd33);
        $finish();
    }
}
```

```ts
import { runTestWithFrontendArtifact } from "@celox-sim/celox";

const result = runTestWithFrontendArtifact(
  artifactJson,
  [{ path: "netlist_tb.veryl", content: testbenchSource }],
  "NetlistTb",
);
```

Here `$sv` denotes Celox's existing external-module link namespace; the design
itself still comes from exactly one external frontend artifact. This compatibility
path does not turn the netlist frontend into a mixed Veryl/SystemVerilog frontend.

Artifacts are validated again when Celox compiles them. Treat the
`format_version` field as the wire-format compatibility boundary and use the SDK
builder instead of constructing JSON by hand.
