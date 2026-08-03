//! Transitional scalar pipeline implemented by the x86 backend.
//!
//! AArch64 temporarily consumes the optimized, pre-allocation x86 MIR while
//! its complete target-owned instruction selection is brought up.
//! This is a compatibility bridge, not a shared-MIR architecture: reusable
//! allocation algorithms move to `celox-backend-common` behind opcode-free
//! facts, while target MIR and machine optimizations remain in each backend.

use std::fmt;

use super::mir::{BlockId, MFunction};
use super::regalloc::{AssignmentMap, RegallocResult};
use super::ssa_destroy::{ParallelCopyOperation, SsaDestructionError, SsaDestructionPlan};

/// Failure while preparing scalar native MIR for a target emitter.
#[derive(Debug)]
pub enum ScalarPrepareError {
    Sir {
        phase: &'static str,
        error: crate::verify::SirVerifyError,
    },
    Mir {
        phase: &'static str,
        error: super::mir_verify::MirVerifyError,
    },
    Regalloc(super::regalloc::RegallocError),
    SsaDestruction(SsaDestructionError),
}

impl fmt::Display for ScalarPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sir { phase, error } => write!(formatter, "{phase}: {error}"),
            Self::Mir { phase, error } => write!(formatter, "{phase}: {error}"),
            Self::Regalloc(error) => error.fmt(formatter),
            Self::SsaDestruction(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ScalarPrepareError {}

impl From<super::regalloc::RegallocError> for ScalarPrepareError {
    fn from(error: super::regalloc::RegallocError) -> Self {
        Self::Regalloc(error)
    }
}

impl From<SsaDestructionError> for ScalarPrepareError {
    fn from(error: SsaDestructionError) -> Self {
        Self::SsaDestruction(error)
    }
}

/// Verified scalar MIR plus allocation and out-of-SSA edge copies.
pub struct PreparedScalarFunction {
    pub function: MFunction,
    pub allocation: AssignmentMap,
    pub spill_frame_size: u32,
    state_size: usize,
    ssa_destruction: SsaDestructionPlan,
}

/// Verified and optimized scalar MIR before any target register allocation.
///
/// A target backend may consume this boundary directly and own spilling,
/// coloring, and SSA destruction. The x86 allocation driver remains available
/// through [`allocate_scalar_mir`] for the native x86 path and compatibility
/// fallbacks.
pub struct PreparedScalarMir {
    pub function: MFunction,
    state_size: usize,
}

impl PreparedScalarMir {
    /// Total byte size of the simulator-owned state preceding backend scratch.
    pub fn state_size(&self) -> usize {
        self.state_size
    }
}

impl PreparedScalarFunction {
    /// Total byte size of the simulator-owned state preceding backend scratch.
    pub fn state_size(&self) -> usize {
        self.state_size
    }

    /// Dependency-ordered copies required on one CFG edge.
    pub fn edge_operations(
        &self,
        predecessor: BlockId,
        successor: BlockId,
    ) -> Option<&[ParallelCopyOperation]> {
        self.ssa_destruction
            .edge(predecessor, successor)
            .map(|edge| edge.operations.as_slice())
    }
}

/// Lower, optimize, allocate, and destroy SSA through the scalar x86 pipeline.
/// Target-specific SIMD selection and final emission are intentionally
/// excluded.
pub fn prepare_scalar_eu(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
) -> Result<PreparedScalarFunction, ScalarPrepareError> {
    let prepared = prepare_scalar_mir(sir_eu, layout, four_state)?;
    allocate_scalar_mir(prepared, label)
}

/// Lower and optimize scalar MIR without performing target register
/// allocation or SSA destruction.
pub fn prepare_scalar_mir(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
) -> Result<PreparedScalarMir, ScalarPrepareError> {
    sir_eu
        .verify_result()
        .map_err(|error| ScalarPrepareError::Sir {
            phase: "at scalar native backend boundary",
            error,
        })?;

    let mut function = super::isel::lower_execution_unit(sir_eu, layout, four_state);
    function
        .verify_result()
        .map_err(|error| ScalarPrepareError::Mir {
            phase: "after scalar instruction selection",
            error,
        })?;
    super::mir_legalize::legalize(&mut function);
    function
        .verify_result()
        .map_err(|error| ScalarPrepareError::Mir {
            phase: "after scalar MIR legalization",
            error,
        })?;
    super::mir_opt::optimize(&mut function);
    super::mir_opt::compact_vregs(&mut function);
    function
        .verify_result()
        .map_err(|error| ScalarPrepareError::Mir {
            phase: "after scalar MIR optimization",
            error,
        })?;

    let state_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native simulation-state size overflow");
    Ok(PreparedScalarMir {
        function,
        state_size,
    })
}

/// Run the mature x86 allocation and out-of-SSA pipeline from the shared
/// pre-allocation scalar MIR boundary.
pub fn allocate_scalar_mir(
    mut prepared: PreparedScalarMir,
    label: &str,
) -> Result<PreparedScalarFunction, ScalarPrepareError> {
    let RegallocResult {
        assignment,
        spill_frame_size,
    } = super::regalloc::run_regalloc_with_label(&mut prepared.function, label)?;
    super::mir_opt::post_regalloc_peephole(&mut prepared.function);
    super::mir_opt::post_regalloc_cleanup(&mut prepared.function);
    super::mir_opt::post_regalloc_direct_load_cse(&mut prepared.function, &assignment);
    super::regalloc::verify_assignment(&prepared.function, &assignment)?;
    prepared
        .function
        .verify_result()
        .map_err(|error| ScalarPrepareError::Mir {
            phase: "after scalar post-allocation cleanup",
            error,
        })?;

    let ssa_destruction = SsaDestructionPlan::build(&prepared.function, &assignment)?;
    ssa_destruction.verify(&prepared.function, &assignment, spill_frame_size)?;

    Ok(PreparedScalarFunction {
        function: prepared.function,
        allocation: assignment,
        spill_frame_size,
        state_size: prepared.state_size,
        ssa_destruction,
    })
}
