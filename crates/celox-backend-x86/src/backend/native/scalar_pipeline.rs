//! Transitional scalar pipeline implemented by the x86 backend.
//!
//! AArch64 temporarily consumes this allocated x86 MIR while its complete
//! target-owned instruction selection and allocation driver are brought up.
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

/// Lower, optimize, allocate, and destroy SSA through the transitional scalar
/// x86 pipeline. Target-specific SIMD selection and final emission are
/// intentionally excluded.
pub fn prepare_scalar_eu(
    sir_eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    layout: &crate::MemoryLayout,
    four_state: bool,
    label: &str,
) -> Result<PreparedScalarFunction, ScalarPrepareError> {
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

    let RegallocResult {
        assignment,
        spill_frame_size,
    } = super::regalloc::run_regalloc_with_label(&mut function, label)?;
    super::mir_opt::post_regalloc_peephole(&mut function);
    super::mir_opt::post_regalloc_cleanup(&mut function);
    super::mir_opt::post_regalloc_direct_load_cse(&mut function, &assignment);
    super::regalloc::verify_assignment(&function, &assignment)?;
    function
        .verify_result()
        .map_err(|error| ScalarPrepareError::Mir {
            phase: "after scalar post-allocation cleanup",
            error,
        })?;

    let ssa_destruction = SsaDestructionPlan::build(&function, &assignment)?;
    ssa_destruction.verify(&function, &assignment, spill_frame_size)?;
    let state_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .expect("native simulation-state size overflow");

    Ok(PreparedScalarFunction {
        function,
        allocation: assignment,
        spill_frame_size,
        state_size,
        ssa_destruction,
    })
}
