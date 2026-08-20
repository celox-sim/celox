use celox_frontend_sdk::{BinaryOp, FrontendArtifact, ModuleBuilder, ValueType};
use celox_napi::NativeSimulatorHandle;
use napi::{Error, Result};
use napi_derive::napi;

/// The frontend's own artifact type. It is not a Celox JSON artifact.
#[napi(object)]
pub struct MyArtifact {
    pub module_name: String,
    pub width: u32,
}

fn lower_to_celox(artifact: MyArtifact) -> Result<FrontendArtifact> {
    let build = || -> std::result::Result<FrontendArtifact, Box<dyn std::error::Error>> {
        let value_type = ValueType::bits(artifact.width as usize)?;
        let mut module = ModuleBuilder::new(artifact.module_name)?;
        let a = module.input("a", value_type)?;
        let b = module.input("b", value_type)?;
        let y = module.output("y", value_type)?;
        let a_expr = module.read(a)?;
        let b_expr = module.read(b)?;
        let sum = module.binary(BinaryOp::Add, a_expr, b_expr, value_type)?;
        let y_target = module.whole(y)?;
        module.assign(y_target, sum)?;
        Ok(module.finish())
    };

    build().map_err(|error| Error::from_reason(error.to_string()))
}

/// A frontend-owned JS constructor. The artifact stays typed from JS through
/// the frontend's Rust lowering code and is never serialized as Celox JSON.
#[napi]
pub fn from_my_artifact(artifact: MyArtifact) -> Result<NativeSimulatorHandle> {
    NativeSimulatorHandle::from_frontend(lower_to_celox(artifact)?, None)
}
