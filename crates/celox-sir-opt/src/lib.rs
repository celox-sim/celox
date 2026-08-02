//! Backend-independent SIR optimization policy and pass pipeline.

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<K> = fxhash::FxHashSet<K>;

/// Source-independent SIR and design vocabulary specialized to semantic state
/// addresses. It deliberately exposes no frontend or physical-layout type.
pub mod ir {
    pub use celox_design::{
        BinaryOp, DomainKind, InstanceId, RegionedAbsoluteAddrBase, RuntimeSchema,
        SPARSE_WORKING_REGION, STABLE_REGION, StateAddr, StateObjectId, TriggerIdWithKind, UnaryOp,
        VarAtomBase, WORKING_REGION,
    };
    pub use celox_sir::*;

    pub type AbsoluteAddr = celox_design::StateAddr;
    pub type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
    pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;

    pub mod cfg {
        pub use celox_sir::cfg::*;
    }

    pub mod verify {
        pub use celox_sir::verify::*;
    }
}

/// Mutable optimization view over source-independent compiler state.
/// Frontend lookup tables and testbench source cannot enter this crate.
pub struct OptimizationContext<'a> {
    pub sir: &'a mut ir::SirProgram,
    pub design: &'a celox_design::ElaboratedDesign<ir::AbsoluteAddr>,
    pub runtime_schema: &'a celox_design::RuntimeSchema<ir::AbsoluteAddr>,
    pub layout_requirements: &'a mut celox_state_layout::LayoutRequirements<ir::AbsoluteAddr>,
}

impl OptimizationContext<'_> {
    pub fn variable_metadata(
        &self,
        address: &ir::AbsoluteAddr,
    ) -> Option<&celox_design::VariableMetadata> {
        self.design.state_objects.get(address)
    }
}

pub mod timing {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn now() -> std::time::Instant {
        std::time::Instant::now()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn now() -> WasmInstant {
        WasmInstant
    }

    #[cfg(target_arch = "wasm32")]
    #[derive(Clone, Copy)]
    pub struct WasmInstant;

    #[cfg(target_arch = "wasm32")]
    impl WasmInstant {
        pub fn elapsed(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    }
}

/// Cost-model threshold for preferring chunked memory shifts.
const MEM_SHIFT_THRESHOLD: usize = 4;

mod error;
mod memory_contract;
pub mod optimizer;
pub use error::{OptimizationError, OptimizationErrorKind};
pub use memory_contract::verify_memory_offset_contract;

pub fn optimize(
    program: &mut OptimizationContext<'_>,
    four_state: bool,
    optimize_options: &OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    optimizer::run(
        program,
        &PassOptions {
            four_state,
            optimize_options: optimize_options.clone(),
            preserve_element_storage_layout,
            ..PassOptions::default()
        },
    );
}

mod policy;
pub use policy::{OptLevel, OptimizeOptions, PassOptions, SirDiagnostics, SirPass};
