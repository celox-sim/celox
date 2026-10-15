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

## Processes and delays

A frontend can add procedural processes, such as `initial` blocks, with
`ModuleBuilder::process`. A process starts at time zero and runs its statements
in order:

| Statement | Behavior |
|---|---|
| `Assign` | Blocking assignment. Later statements read the new value; other logic sees it once the process suspends. |
| `If` | Branch on a condition. A condition with unknown bits counts as false. |
| `While` / `Forever` | Repeat a body while a condition holds, or forever. |
| `Delay` | Suspend for an amount of time units, at most 64 bits wide. |
| `ClockCycles` | Suspend until a number of rising edges of a clock have passed. The clock must clock a register and have a period set with `ModuleBuilder::clock_period`; the runtime generates its edges from the first wait on it, and the process resumes one period after the last counted edge, before the edge that follows. |
| `Finish` | End the simulation. |

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

Processes run only in a timed `Simulation`. Each one is compiled into a
resumable kernel that the simulation scheduler resumes when its delay expires.
Processes that resume at the same time run in declaration order. A process
sees the state settled at the previous time. The clock and reset edges it
causes trigger registers at the current time, like a scheduled event. A zero
delay resumes the process later at the same time, after the other processes
have run and the registers their edges trigger have settled, so a pulse
separated by a zero delay is still an edge.

A clock that processes wait on with `ClockCycles` toggles every half period
once a process has waited on it, as long as one does. While every live
process waits on the same clock and nothing else is scheduled, the edges run
as fused ticks inside the backend's generated code, so a long wait costs no
more than the same number of ticks of an event-driven `Simulator`; such a
run skips the clock signal's own edges and the per-edge hooks of external
components, which are honored again as soon as another process or event
interleaves.

A process may write output and internal signals that no continuous assignment
or register drives. Several processes may write the same signal. Checkpoints
capture suspended processes, but state files are not yet supported for a
simulation with processes. A Veryl native testbench cannot instantiate an
artifact with processes.

## Artifact format limits

Format version 1 accepts one flattened module. It supports typed signals,
constants, combinational expressions and assignments, edge-triggered registers,
asynchronous reset, synchronous enable, and initial values. Format version 2
adds processes. The builder writes version 1 for an artifact without processes,
so consumers that read only version 1 still accept it. A frontend must lower
hierarchy, memories, latches, custom primitives, and bidirectional signals
before calling the SDK builder.

Celox validates the artifact again before compilation. Produce it through the
SDK builder and pass the Rust value directly to `Simulator::from_frontend`.
