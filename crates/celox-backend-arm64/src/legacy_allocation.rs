//! Transitional lowering from the established x86 MIR and allocator result.
//!
//! No AArch64 emitter code should interpret x86 opcodes or register colors
//! directly. This module is deleted once AArch64 instruction selection and
//! allocation produce the target-owned MIR without a legacy input.

use std::fmt;

use celox_backend_x86::native::mir as legacy_mir;
use celox_backend_x86::native::regalloc::AssignmentMap as LegacyAssignment;
use celox_backend_x86::native::regalloc::assignment::EdgeLocation;
use celox_backend_x86::native::ssa_destroy::{SsaDestructionError, SsaDestructionPlan};

use crate::Arm64Reg;
use crate::allocation::{CopyDestination, CopyOperation, CopySource, EdgeCopyPlan};
use crate::mir::{self, AllocatedFunction};

#[derive(Debug)]
pub(crate) enum LegacyLoweringError {
    Ssa(SsaDestructionError),
    TargetAllocation(crate::regalloc::TargetRegallocError),
    Unsupported(&'static str),
}

impl fmt::Display for LegacyLoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ssa(error) => error.fmt(formatter),
            Self::TargetAllocation(error) => error.fmt(formatter),
            Self::Unsupported(instruction) => {
                write!(formatter, "AArch64 lowering does not support {instruction}")
            }
        }
    }
}

impl std::error::Error for LegacyLoweringError {}

impl From<SsaDestructionError> for LegacyLoweringError {
    fn from(error: SsaDestructionError) -> Self {
        Self::Ssa(error)
    }
}

impl From<crate::regalloc::TargetRegallocError> for LegacyLoweringError {
    fn from(error: crate::regalloc::TargetRegallocError) -> Self {
        Self::TargetAllocation(error)
    }
}

pub(crate) fn adapt(
    function: &legacy_mir::MFunction,
    assignment: &LegacyAssignment,
    spill_frame_size: u32,
) -> Result<AllocatedFunction, LegacyLoweringError> {
    let legacy_plan = SsaDestructionPlan::build(function, assignment)?;
    legacy_plan.verify(function, assignment, spill_frame_size)?;

    // The legacy plan proves that every pre-existing spill home and phi source
    // is complete. Physical register colors are deliberately discarded below.
    let blocks = function
        .blocks
        .iter()
        .map(|block| {
            Ok(mir::MBlock {
                id: adapt_block(block.id),
                phis: block
                    .phis
                    .iter()
                    .map(|phi| mir::PhiNode {
                        dst: adapt_vreg(phi.dst),
                        sources: phi
                            .sources
                            .iter()
                            .map(|&(predecessor, value)| {
                                (adapt_block(predecessor), adapt_vreg(value))
                            })
                            .collect(),
                    })
                    .collect(),
                insts: block
                    .insts
                    .iter()
                    .map(adapt_instruction)
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<_, LegacyLoweringError>>()?;
    let mut target = crate::regalloc::allocate_without_spills(mir::MFunction::new(
        blocks,
        function.constant_tables().to_vec(),
    ))?;
    target.edge_copies = rebuild_legacy_home_copies(function, assignment, &target)?;
    Ok(target)
}

fn rebuild_legacy_home_copies(
    function: &legacy_mir::MFunction,
    legacy_assignment: &LegacyAssignment,
    target: &AllocatedFunction,
) -> Result<EdgeCopyPlan<mir::BlockId>, LegacyLoweringError> {
    use celox_backend_common::regalloc as common;

    let mut plan = EdgeCopyPlan::default();
    for successor in &function.blocks {
        let mut rows_by_predecessor = std::collections::BTreeMap::<
            legacy_mir::BlockId,
            Vec<common::ParallelCopy<Arm64Reg>>,
        >::new();
        for phi in &successor.phis {
            if legacy_assignment.is_semantic_phi_definition(phi.dst) {
                continue;
            }
            let destination = if let Some(offset) = legacy_assignment.edge_spill_slot(phi.dst) {
                common::CopyDestination::Stack(offset)
            } else {
                common::CopyDestination::Register(
                    target.assignment.get(&adapt_vreg(phi.dst)).ok_or(
                        crate::regalloc::TargetRegallocError::MissingAssignment {
                            block: adapt_block(successor.id),
                            instruction: 0,
                            value: adapt_vreg(phi.dst),
                        },
                    )?,
                )
            };
            for &(predecessor, source_value) in &phi.sources {
                let source = match legacy_assignment.resolved_phi_source_location(
                    predecessor,
                    successor.id,
                    phi.dst,
                    source_value,
                ) {
                    Some(EdgeLocation::Stack(offset)) => common::CopySource::Stack(offset),
                    Some(EdgeLocation::Immediate(value)) => common::CopySource::Immediate(value),
                    Some(EdgeLocation::Register(_)) => common::CopySource::Register(
                        target.assignment.get(&adapt_vreg(source_value)).ok_or(
                            crate::regalloc::TargetRegallocError::MissingAssignment {
                                block: adapt_block(predecessor),
                                instruction: 0,
                                value: adapt_vreg(source_value),
                            },
                        )?,
                    ),
                    None => {
                        return Err(SsaDestructionError {
                            rule: "SSA_DEST.MISSING_PHI_SOURCE_LOCATION",
                            predecessor: Some(predecessor),
                            successor: Some(successor.id),
                            phi_destination: Some(phi.dst),
                            source_value: Some(source_value),
                            message: "phi source has no completed legacy edge location".into(),
                        }
                        .into());
                    }
                };
                rows_by_predecessor
                    .entry(predecessor)
                    .or_default()
                    .push(common::ParallelCopy {
                        destination,
                        source,
                    });
            }
        }
        for (predecessor, rows) in rows_by_predecessor {
            let resolution = common::resolve_parallel_copies(&rows).map_err(|error| {
                crate::regalloc::TargetRegallocError::ParallelCopy {
                    predecessor: adapt_block(predecessor),
                    successor: adapt_block(successor.id),
                    message: error.to_string(),
                }
            })?;
            let operations = resolution
                .operations
                .into_iter()
                .map(|operation| match operation {
                    common::CopyOperation::Move {
                        destination,
                        source,
                    } => CopyOperation::Move {
                        destination: adapt_common_destination(destination),
                        source: adapt_common_source(source),
                    },
                    common::CopyOperation::SwapRegisters { left, right } => {
                        CopyOperation::SwapRegisters { left, right }
                    }
                    common::CopyOperation::SaveTemporary(destination) => {
                        CopyOperation::SaveTemporary(adapt_common_destination(destination))
                    }
                    common::CopyOperation::RestoreTemporary(destination) => {
                        CopyOperation::RestoreTemporary(adapt_common_destination(destination))
                    }
                })
                .collect();
            plan.insert(
                adapt_block(predecessor),
                adapt_block(successor.id),
                operations,
            );
        }
    }
    Ok(plan)
}

fn adapt_common_destination(
    destination: celox_backend_common::regalloc::CopyDestination<Arm64Reg>,
) -> CopyDestination {
    match destination {
        celox_backend_common::regalloc::CopyDestination::Register(register) => {
            CopyDestination::Register(register)
        }
        celox_backend_common::regalloc::CopyDestination::Stack(offset) => {
            CopyDestination::Stack(offset)
        }
    }
}

fn adapt_common_source(source: celox_backend_common::regalloc::CopySource<Arm64Reg>) -> CopySource {
    match source {
        celox_backend_common::regalloc::CopySource::Register(register) => {
            CopySource::Register(register)
        }
        celox_backend_common::regalloc::CopySource::Stack(offset) => CopySource::Stack(offset),
        celox_backend_common::regalloc::CopySource::Immediate(value) => {
            CopySource::Immediate(value)
        }
    }
}

fn adapt_vreg(value: legacy_mir::VReg) -> mir::VReg {
    mir::VReg(value.0)
}

fn adapt_block(block: legacy_mir::BlockId) -> mir::BlockId {
    mir::BlockId(block.0)
}

fn adapt_size(size: legacy_mir::OpSize) -> mir::OpSize {
    match size {
        legacy_mir::OpSize::S8 => mir::OpSize::S8,
        legacy_mir::OpSize::S16 => mir::OpSize::S16,
        legacy_mir::OpSize::S32 => mir::OpSize::S32,
        legacy_mir::OpSize::S64 => mir::OpSize::S64,
    }
}

fn adapt_base(base: legacy_mir::BaseReg) -> mir::BaseReg {
    match base {
        legacy_mir::BaseReg::SimState => mir::BaseReg::SimState,
        legacy_mir::BaseReg::StackFrame => mir::BaseReg::StackFrame,
    }
}

fn adapt_cmp(kind: legacy_mir::CmpKind) -> mir::CmpKind {
    match kind {
        legacy_mir::CmpKind::Eq => mir::CmpKind::Eq,
        legacy_mir::CmpKind::Ne => mir::CmpKind::Ne,
        legacy_mir::CmpKind::LtU => mir::CmpKind::LtU,
        legacy_mir::CmpKind::LtS => mir::CmpKind::LtS,
        legacy_mir::CmpKind::LeU => mir::CmpKind::LeU,
        legacy_mir::CmpKind::LeS => mir::CmpKind::LeS,
        legacy_mir::CmpKind::GtU => mir::CmpKind::GtU,
        legacy_mir::CmpKind::GtS => mir::CmpKind::GtS,
        legacy_mir::CmpKind::GeU => mir::CmpKind::GeU,
        legacy_mir::CmpKind::GeS => mir::CmpKind::GeS,
    }
}

fn adapt_predicate(predicate: legacy_mir::BranchPredicate) -> mir::BranchPredicate {
    match predicate {
        legacy_mir::BranchPredicate::Compare { lhs, rhs, kind } => mir::BranchPredicate::Compare {
            lhs: adapt_vreg(lhs),
            rhs: adapt_vreg(rhs),
            kind: adapt_cmp(kind),
        },
        legacy_mir::BranchPredicate::CompareImm { lhs, imm, kind } => {
            mir::BranchPredicate::CompareImm {
                lhs: adapt_vreg(lhs),
                imm,
                kind: adapt_cmp(kind),
            }
        }
        legacy_mir::BranchPredicate::MemoryNonZero { base, offset, size } => {
            mir::BranchPredicate::MemoryNonZero {
                base: adapt_base(base),
                offset,
                size: adapt_size(size),
            }
        }
    }
}

fn adapt_lane_rhs(rhs: legacy_mir::PackedLaneCompareRhs) -> mir::PackedLaneCompareRhs {
    match rhs {
        legacy_mir::PackedLaneCompareRhs::Scalar(value) => {
            mir::PackedLaneCompareRhs::Scalar(adapt_vreg(value))
        }
        legacy_mir::PackedLaneCompareRhs::Memory { offset, .. } => {
            mir::PackedLaneCompareRhs::Memory { offset }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn adapt_instruction(instruction: &legacy_mir::MInst) -> Result<mir::MInst, LegacyLoweringError> {
    use legacy_mir::MInst as L;
    use mir::MInst as A;

    let instruction =
        match instruction {
            L::X86Simd(_) => return Err(LegacyLoweringError::Unsupported("x86 SIMD MIR")),
            L::Mov { dst, src } => A::Mov {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
            },
            L::Mov32 { dst, src } => A::Mov32 {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
            },
            L::LoadImm { dst, value } => A::LoadImm {
                dst: adapt_vreg(*dst),
                value: *value,
            },
            L::Scratch { dst } => A::Scratch {
                dst: adapt_vreg(*dst),
            },
            L::LoadConstantTableAddr { dst, table } => A::LoadConstantTableAddr {
                dst: adapt_vreg(*dst),
                table: mir::ConstantTableId(table.0),
            },
            L::Load {
                dst,
                base,
                offset,
                size,
            } => A::Load {
                dst: adapt_vreg(*dst),
                base: adapt_base(*base),
                offset: *offset,
                size: adapt_size(*size),
            },
            L::Store {
                base,
                offset,
                src,
                size,
            } => A::Store {
                base: adapt_base(*base),
                offset: *offset,
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::AndStoreImm {
                base,
                offset,
                size,
                imm,
            } => A::AndStoreImm {
                base: adapt_base(*base),
                offset: *offset,
                size: adapt_size(*size),
                imm: *imm,
            },
            L::OrStoreImm {
                base,
                offset,
                size,
                imm,
            } => A::OrStoreImm {
                base: adapt_base(*base),
                offset: *offset,
                size: adapt_size(*size),
                imm: *imm,
            },
            L::LoadPtr {
                dst,
                ptr,
                offset,
                size,
            } => A::LoadPtr {
                dst: adapt_vreg(*dst),
                ptr: adapt_vreg(*ptr),
                offset: *offset,
                size: adapt_size(*size),
            },
            L::StorePtr {
                ptr,
                offset,
                src,
                size,
            } => A::StorePtr {
                ptr: adapt_vreg(*ptr),
                offset: *offset,
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::ReleaseStorePtr {
                ptr,
                offset,
                src,
                size,
            } => A::ReleaseStorePtr {
                ptr: adapt_vreg(*ptr),
                offset: *offset,
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::LoadIndexed {
                dst,
                base,
                offset,
                index,
                scale,
                size,
                ..
            } => A::LoadIndexed {
                dst: adapt_vreg(*dst),
                base: adapt_base(*base),
                offset: *offset,
                index: adapt_vreg(*index),
                scale: *scale,
                size: adapt_size(*size),
            },
            L::PackedLaneCompare {
                dst,
                rhs,
                kind,
                offset,
                lane_count,
                element_stride,
                bit_offset,
                field_width,
                ..
            } => A::PackedLaneCompare {
                dst: adapt_vreg(*dst),
                rhs: adapt_lane_rhs(*rhs),
                kind: adapt_cmp(*kind),
                offset: *offset,
                lane_count: *lane_count,
                element_stride: *element_stride,
                bit_offset: *bit_offset,
                field_width: *field_width,
            },
            L::PackedByteAffineCompare {
                dst,
                base,
                rhs,
                kind,
            } => A::PackedByteAffineCompare {
                dst: adapt_vreg(*dst),
                base: adapt_vreg(*base),
                rhs: adapt_vreg(*rhs),
                kind: adapt_cmp(*kind),
            },
            L::StoreIndexed {
                base,
                offset,
                index,
                src,
                size,
                ..
            } => A::StoreIndexed {
                base: adapt_base(*base),
                offset: *offset,
                index: adapt_vreg(*index),
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::OrStoreIndexed {
                base,
                offset,
                index,
                src,
                size,
                ..
            } => A::OrStoreIndexed {
                base: adapt_base(*base),
                offset: *offset,
                index: adapt_vreg(*index),
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::LoadPtrIndexed {
                dst,
                ptr,
                offset,
                index,
                size,
            } => A::LoadPtrIndexed {
                dst: adapt_vreg(*dst),
                ptr: adapt_vreg(*ptr),
                offset: *offset,
                index: adapt_vreg(*index),
                size: adapt_size(*size),
            },
            L::StorePtrIndexed {
                ptr,
                offset,
                index,
                src,
                size,
            } => A::StorePtrIndexed {
                ptr: adapt_vreg(*ptr),
                offset: *offset,
                index: adapt_vreg(*index),
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::ReleaseStorePtrIndexed {
                ptr,
                offset,
                index,
                src,
                size,
            } => A::ReleaseStorePtrIndexed {
                ptr: adapt_vreg(*ptr),
                offset: *offset,
                index: adapt_vreg(*index),
                src: adapt_vreg(*src),
                size: adapt_size(*size),
            },
            L::MemCopy {
                src_offset,
                dst_offset,
                byte_len,
            } => A::MemCopy {
                src_offset: *src_offset,
                dst_offset: *dst_offset,
                byte_len: *byte_len,
            },
            L::MemFill {
                dst_offset,
                byte_len,
                value,
            } => A::MemFill {
                dst_offset: *dst_offset,
                byte_len: *byte_len,
                value: *value,
            },
            L::SparseCommit {
                src_offset,
                dst_offset,
                byte_size,
                dirty_words_offset,
                dirty_word_count,
                summary_words_offset,
                summary_word_count,
                four_state,
            } => A::SparseCommit {
                src_offset: *src_offset,
                dst_offset: *dst_offset,
                byte_size: *byte_size,
                dirty_words_offset: *dirty_words_offset,
                dirty_word_count: *dirty_word_count,
                summary_words_offset: *summary_words_offset,
                summary_word_count: *summary_word_count,
                four_state: *four_state,
            },
            L::SparseMarkActive {
                active_index,
                active_bits_offset,
                active_capacity,
            } => A::SparseMarkActive {
                active_index: *active_index,
                active_bits_offset: *active_bits_offset,
                active_capacity: *active_capacity,
            },
            L::SparseCommitWorklist {
                descriptor_table,
                active_bits_offset,
                active_capacity,
            } => A::SparseCommitWorklist {
                descriptor_table: mir::ConstantTableId(descriptor_table.0),
                active_bits_offset: *active_bits_offset,
                active_capacity: *active_capacity,
            },
            L::Add { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Add { dst, lhs, rhs })
            }
            L::Add32 { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Add32 { dst, lhs, rhs })
            }
            L::Sub { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Sub { dst, lhs, rhs })
            }
            L::Sub32 { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Sub32 { dst, lhs, rhs })
            }
            L::Mul { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Mul { dst, lhs, rhs })
            }
            L::Mul32 { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Mul32 { dst, lhs, rhs })
            }
            L::UMulHi { dst, lhs, rhs } => adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| {
                A::UMulHi { dst, lhs, rhs }
            }),
            L::And { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::And { dst, lhs, rhs })
            }
            L::And32 { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::And32 { dst, lhs, rhs })
            }
            L::Or { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Or { dst, lhs, rhs })
            }
            L::Or32 { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Or32 { dst, lhs, rhs })
            }
            L::Xor { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Xor { dst, lhs, rhs })
            }
            L::Xor32 { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Xor32 { dst, lhs, rhs })
            }
            L::Shr { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Shr { dst, lhs, rhs })
            }
            L::Shl { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Shl { dst, lhs, rhs })
            }
            L::Sar { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::Sar { dst, lhs, rhs })
            }
            L::AndImm { dst, src, imm } => A::AndImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::AndImm32 { dst, src, imm } => A::AndImm32 {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::OrImm { dst, src, imm } => A::OrImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::ShrImm { dst, src, imm } => A::ShrImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::ShlImm { dst, src, imm } => A::ShlImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::SarImm { dst, src, imm } => A::SarImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::AddImm { dst, src, imm } => A::AddImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::SubImm { dst, src, imm } => A::SubImm {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                imm: *imm,
            },
            L::Cmp {
                dst,
                lhs,
                rhs,
                kind,
            } => A::Cmp {
                dst: adapt_vreg(*dst),
                lhs: adapt_vreg(*lhs),
                rhs: adapt_vreg(*rhs),
                kind: adapt_cmp(*kind),
            },
            L::CmpImm {
                dst,
                lhs,
                imm,
                kind,
            } => A::CmpImm {
                dst: adapt_vreg(*dst),
                lhs: adapt_vreg(*lhs),
                imm: *imm,
                kind: adapt_cmp(*kind),
            },
            L::UDiv { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::UDiv { dst, lhs, rhs })
            }
            L::URem { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::URem { dst, lhs, rhs })
            }
            L::SDiv { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::SDiv { dst, lhs, rhs })
            }
            L::SRem { dst, lhs, rhs } => {
                adapt_binary(*dst, *lhs, *rhs, |dst, lhs, rhs| A::SRem { dst, lhs, rhs })
            }
            L::BitNot { dst, src } => adapt_unary(*dst, *src, |dst, src| A::BitNot { dst, src }),
            L::Neg { dst, src } => adapt_unary(*dst, *src, |dst, src| A::Neg { dst, src }),
            L::Popcnt { dst, src } => adapt_unary(*dst, *src, |dst, src| A::Popcnt { dst, src }),
            L::Bsf { dst, src } => adapt_unary(*dst, *src, |dst, src| A::Bsf { dst, src }),
            L::Bsr { dst, src } => adapt_unary(*dst, *src, |dst, src| A::Bsr { dst, src }),
            L::BsrOr {
                dst,
                src,
                zero_value,
            } => A::BsrOr {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                zero_value: *zero_value,
            },
            L::Pext { dst, src, mask } => A::Pext {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                mask: adapt_vreg(*mask),
            },
            L::Pdep { dst, src, mask } => A::Pdep {
                dst: adapt_vreg(*dst),
                src: adapt_vreg(*src),
                mask: adapt_vreg(*mask),
            },
            L::Select {
                dst,
                cond,
                true_val,
                false_val,
            } => A::Select {
                dst: adapt_vreg(*dst),
                cond: adapt_vreg(*cond),
                true_val: adapt_vreg(*true_val),
                false_val: adapt_vreg(*false_val),
            },
            L::CmpSelect {
                dst,
                lhs,
                rhs,
                kind,
                true_val,
                false_val,
            } => A::CmpSelect {
                dst: adapt_vreg(*dst),
                lhs: adapt_vreg(*lhs),
                rhs: adapt_vreg(*rhs),
                kind: adapt_cmp(*kind),
                true_val: adapt_vreg(*true_val),
                false_val: adapt_vreg(*false_val),
            },
            L::CmpImmSelect {
                dst,
                lhs,
                imm,
                kind,
                true_val,
                false_val,
            } => A::CmpImmSelect {
                dst: adapt_vreg(*dst),
                lhs: adapt_vreg(*lhs),
                imm: *imm,
                kind: adapt_cmp(*kind),
                true_val: adapt_vreg(*true_val),
                false_val: adapt_vreg(*false_val),
            },
            L::GuardedCmpSelect {
                dst,
                guard,
                lhs,
                rhs,
                kind,
                true_val,
                false_val,
            } => A::GuardedCmpSelect {
                dst: adapt_vreg(*dst),
                guard: adapt_vreg(*guard),
                lhs: adapt_vreg(*lhs),
                rhs: adapt_vreg(*rhs),
                kind: adapt_cmp(*kind),
                true_val: adapt_vreg(*true_val),
                false_val: adapt_vreg(*false_val),
            },
            L::Branch {
                cond,
                true_bb,
                false_bb,
            } => A::Branch {
                cond: adapt_vreg(*cond),
                true_bb: adapt_block(*true_bb),
                false_bb: adapt_block(*false_bb),
            },
            L::BranchPred {
                predicate,
                true_bb,
                false_bb,
            } => A::BranchPred {
                predicate: adapt_predicate(*predicate),
                true_bb: adapt_block(*true_bb),
                false_bb: adapt_block(*false_bb),
            },
            L::JumpTable { index, targets, .. } => A::JumpTable {
                index: adapt_vreg(*index),
                targets: targets.iter().copied().map(adapt_block).collect(),
            },
            L::Jump { target } => A::Jump {
                target: adapt_block(*target),
            },
            L::Return => A::Return,
            L::ReturnError { code } => A::ReturnError { code: *code },
        };
    Ok(instruction)
}

fn adapt_binary(
    dst: legacy_mir::VReg,
    lhs: legacy_mir::VReg,
    rhs: legacy_mir::VReg,
    make: impl FnOnce(mir::VReg, mir::VReg, mir::VReg) -> mir::MInst,
) -> mir::MInst {
    make(adapt_vreg(dst), adapt_vreg(lhs), adapt_vreg(rhs))
}

fn adapt_unary(
    dst: legacy_mir::VReg,
    src: legacy_mir::VReg,
    make: impl FnOnce(mir::VReg, mir::VReg) -> mir::MInst,
) -> mir::MInst {
    make(adapt_vreg(dst), adapt_vreg(src))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_semantic_operands_into_aarch64_owned_types() {
        let lowered = adapt_instruction(&legacy_mir::MInst::LoadIndexed {
            dst: legacy_mir::VReg(3),
            base: legacy_mir::BaseReg::StackFrame,
            offset: -24,
            index: legacy_mir::VReg(7),
            scale: 8,
            size: legacy_mir::OpSize::S32,
            alias_range: None,
        })
        .unwrap();

        assert_eq!(
            lowered,
            mir::MInst::LoadIndexed {
                dst: mir::VReg(3),
                base: mir::BaseReg::StackFrame,
                offset: -24,
                index: mir::VReg(7),
                scale: 8,
                size: mir::OpSize::S32,
            }
        );
    }

    #[test]
    fn rejects_x86_only_opcodes_at_the_legacy_boundary() {
        let error = adapt_instruction(&legacy_mir::MInst::X86Simd(
            legacy_mir::X86SimdInst::Zero128 {
                dst: legacy_mir::X86VecReg(0),
            },
        ))
        .unwrap_err();

        assert!(matches!(
            error,
            LegacyLoweringError::Unsupported("x86 SIMD MIR")
        ));
    }

    #[test]
    fn recolors_legacy_mir_with_the_aarch64_register_file() {
        use celox_backend_x86::native::regalloc::assignment::PhysReg;

        let mut values = legacy_mir::VRegAllocator::new();
        let lhs = values.alloc();
        let rhs = values.alloc();
        let result = values.alloc();
        let mut function =
            legacy_mir::MFunction::new(values, vec![legacy_mir::SpillDesc::transient(); 3]);
        let mut block = legacy_mir::MBlock::new(legacy_mir::BlockId(0));
        block.push(legacy_mir::MInst::LoadImm { dst: lhs, value: 2 });
        block.push(legacy_mir::MInst::LoadImm { dst: rhs, value: 3 });
        block.push(legacy_mir::MInst::Add {
            dst: result,
            lhs,
            rhs,
        });
        block.push(legacy_mir::MInst::Return);
        function.push_block(block);
        let mut legacy_assignment = LegacyAssignment::default();
        legacy_assignment.set(lhs, PhysReg::R13);
        legacy_assignment.set(rhs, PhysReg::R14);
        legacy_assignment.set(result, PhysReg::R15);

        let target = adapt(&function, &legacy_assignment, 0).unwrap();
        let target_registers = [lhs, rhs, result]
            .map(|value| target.assignment.get(&adapt_vreg(value)).unwrap().number());

        assert!(target_registers.into_iter().all(|register| register <= 2));
    }
}
