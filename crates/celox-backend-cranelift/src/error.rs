use cranelift::codegen::{CodegenError, settings::SetError};
use cranelift_module::ModuleError;
use thiserror::Error;

/// Failure while configuring or compiling with the Cranelift backend.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CraneliftError {
    #[error("failed to set Cranelift option `{option}`: {source}")]
    Setting {
        option: &'static str,
        #[source]
        source: SetError,
    },

    #[error("failed to detect the native target: {message}")]
    NativeTarget { message: &'static str },

    #[error("failed to build the native target ISA: {source}")]
    TargetIsa {
        #[source]
        source: CodegenError,
    },

    #[error("failed to optimize {label}: {source}")]
    Optimize {
        label: String,
        #[source]
        source: CodegenError,
    },

    #[error("{operation}: {source}")]
    Module {
        operation: String,
        #[source]
        source: Box<ModuleError>,
    },
}

impl CraneliftError {
    pub(crate) fn setting(option: &'static str, source: SetError) -> Self {
        Self::Setting { option, source }
    }

    pub(crate) fn optimize(label: impl Into<String>, source: CodegenError) -> Self {
        Self::Optimize {
            label: label.into(),
            source,
        }
    }

    pub(crate) fn module(operation: impl Into<String>, source: ModuleError) -> Self {
        Self::Module {
            operation: operation.into(),
            source: Box::new(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use cranelift::codegen::settings::SetError;

    use super::CraneliftError;

    #[test]
    fn setting_error_preserves_its_source() {
        let error = CraneliftError::setting("opt_level", SetError::BadValue("enum".into()));

        assert!(error.to_string().contains("Cranelift option `opt_level`"));
        assert_eq!(
            error.source().unwrap().to_string(),
            "Unexpected value for a setting, expected enum"
        );
    }
}
