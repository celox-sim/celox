# celox-napi

Rust adapter API and N-API/WASI handles for Celox. This crate is useful to
frontend authors that ship their own native addon and want to expose a
frontend-specific constructor without serializing a Celox artifact as JSON.

The frontend addon accepts its own artifact type, lowers it with
`celox-frontend-sdk`, and returns the standard handle:

```rust
use celox_frontend_sdk::FrontendArtifact;
use celox_napi::NativeSimulatorHandle;
use napi::Result;
use napi_derive::napi;

#[napi(object)]
pub struct MyArtifact {
    // Fields owned by this frontend.
    pub module_name: String,
}

fn lower_to_celox(artifact: MyArtifact) -> Result<FrontendArtifact> {
    # todo!()
}

#[napi]
pub fn from_my_artifact(artifact: MyArtifact) -> Result<NativeSimulatorHandle> {
    NativeSimulatorHandle::from_frontend(lower_to_celox(artifact)?, None)
}
```

The frontend's TypeScript package can expose the API its users expect:

```ts
import { Simulator, type FrontendSimulatorHandle } from "@celox-sim/celox";
import { fromMyArtifact as nativeFromMyArtifact } from "./native.js";

export function fromMyArtifact<P>(artifact: MyArtifact): Simulator<P> {
  const handle: FrontendSimulatorHandle = nativeFromMyArtifact(artifact);
  return Simulator.fromFrontendHandle<P>(handle);
}
```

`MyArtifact` remains the frontend's public model. Its conversion happens inside
Rust, directly into `FrontendArtifact`; the frontend artifact is not transported
as JSON.

See `examples/my-frontend-napi` in the Celox repository for a buildable addon.
