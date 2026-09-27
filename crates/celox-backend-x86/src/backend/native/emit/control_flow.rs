//! Parallel copies, block layout, labels, and conditional branches.

use super::*;

// ────────────────────────────────────────────────────────────────
// Verified parallel-copy lowering
// ────────────────────────────────────────────────────────────────

/// Lower a pre-validated edge plan. This function deliberately has no access
/// to MIR phi nodes or the assignment map: all semantic decisions belong to
/// SSA destruction planning and verification, before x86 encoding starts.
pub(super) fn emit_parallel_copy_plan(
    asm: &mut CodeAssembler,
    edge: Option<&EdgeCopyPlan>,
) -> Result<(), EmitError> {
    let Some(edge) = edge else {
        return Ok(());
    };

    let mut temporary_live = false;
    for operation in &edge.operations {
        match *operation {
            ParallelCopyOperation::Move {
                destination,
                source,
            } => {
                emit_single_parallel_copy(asm, destination, source, 0)?;
            }
            ParallelCopyOperation::SwapRegisters { left, right } => {
                if temporary_live {
                    return Err(parallel_copy_input_error(
                        "EMIT.PARALLEL_COPY_TEMPORARY",
                        "parallel-copy schedule exchanges registers while a temporary is live",
                    ));
                }
                asm.xchg(preg_to_reg64(left), preg_to_reg64(right))?;
            }
            ParallelCopyOperation::SaveTemporary(location) => {
                if temporary_live {
                    return Err(parallel_copy_input_error(
                        "EMIT.PARALLEL_COPY_TEMPORARY",
                        "parallel-copy schedule nests temporary saves",
                    ));
                }
                match location {
                    ParallelCopyDestination::Register(register) => {
                        asm.mov(qword_ptr(scratch_operand(0)), preg_to_reg64(register))?
                    }
                    ParallelCopyDestination::Stack(slot) => {
                        let offset = checked_parallel_copy_offset(slot, 0)?;
                        asm.movq(xmm0, qword_ptr(mem_operand(BaseReg::StackFrame, offset)))?;
                        asm.movq(qword_ptr(scratch_operand(0)), xmm0)?;
                    }
                }
                temporary_live = true;
            }
            ParallelCopyOperation::RestoreTemporary(location) => {
                if !temporary_live {
                    return Err(parallel_copy_input_error(
                        "EMIT.PARALLEL_COPY_TEMPORARY",
                        "parallel-copy schedule restores an inactive temporary",
                    ));
                }
                match location {
                    ParallelCopyDestination::Register(register) => {
                        asm.mov(preg_to_reg64(register), qword_ptr(scratch_operand(0)))?
                    }
                    ParallelCopyDestination::Stack(slot) => {
                        let offset = checked_parallel_copy_offset(slot, 0)?;
                        asm.movq(xmm0, qword_ptr(scratch_operand(0)))?;
                        asm.movq(qword_ptr(mem_operand(BaseReg::StackFrame, offset)), xmm0)?;
                    }
                }
                temporary_live = false;
            }
        }
    }
    if temporary_live {
        return Err(parallel_copy_input_error(
            "EMIT.PARALLEL_COPY_TEMPORARY",
            "parallel-copy schedule leaves a temporary live",
        ));
    }
    Ok(())
}

fn emit_single_parallel_copy(
    asm: &mut CodeAssembler,
    destination: ParallelCopyDestination,
    source: ParallelCopySource,
    stack_adjustment: i32,
) -> Result<(), EmitError> {
    match (destination, source) {
        (ParallelCopyDestination::Register(dst), ParallelCopySource::Register(src)) => {
            asm.mov(preg_to_reg64(dst), preg_to_reg64(src))?;
        }
        (ParallelCopyDestination::Register(dst), ParallelCopySource::Stack(slot)) => {
            let offset = checked_parallel_copy_offset(slot, stack_adjustment)?;
            asm.mov(
                preg_to_reg64(dst),
                qword_ptr(mem_operand(BaseReg::StackFrame, offset)),
            )?;
        }
        (ParallelCopyDestination::Register(dst), ParallelCopySource::Immediate(value)) => {
            asm.mov(preg_to_reg64(dst), value)?;
        }
        (ParallelCopyDestination::Stack(slot), ParallelCopySource::Register(src)) => {
            let offset = checked_parallel_copy_offset(slot, stack_adjustment)?;
            asm.mov(
                qword_ptr(mem_operand(BaseReg::StackFrame, offset)),
                preg_to_reg64(src),
            )?;
        }
        (ParallelCopyDestination::Stack(dst), ParallelCopySource::Stack(src)) => {
            // XMM0 is not part of the GPR allocator and SSE2 is baseline on
            // x86-64, so it is a safe non-stack scratch for a qword memcopy.
            let source_offset = checked_parallel_copy_offset(src, stack_adjustment)?;
            let destination_offset = checked_parallel_copy_offset(dst, stack_adjustment)?;
            asm.movq(
                xmm0,
                qword_ptr(mem_operand(BaseReg::StackFrame, source_offset)),
            )?;
            asm.movq(
                qword_ptr(mem_operand(BaseReg::StackFrame, destination_offset)),
                xmm0,
            )?;
        }
        (ParallelCopyDestination::Stack(slot), ParallelCopySource::Immediate(value)) => {
            // x86 has no arbitrary imm64-to-memory encoding.  Two independent
            // dword stores avoid borrowing an allocatable GPR or stack scratch.
            let low_offset = checked_parallel_copy_offset(slot, stack_adjustment)?;
            let high_adjustment = stack_adjustment.checked_add(4).ok_or_else(|| {
                parallel_copy_input_error(
                    "EMIT.PARALLEL_COPY_OFFSET",
                    "parallel-copy immediate high-word adjustment exceeds i32",
                )
            })?;
            let high_offset = checked_parallel_copy_offset(slot, high_adjustment)?;
            asm.mov(
                dword_ptr(mem_operand(BaseReg::StackFrame, low_offset)),
                value as u32,
            )?;
            asm.mov(
                dword_ptr(mem_operand(BaseReg::StackFrame, high_offset)),
                (value >> 32) as u32,
            )?;
        }
    }
    Ok(())
}

fn checked_parallel_copy_offset(slot: i32, adjustment: i32) -> Result<i32, EmitError> {
    slot.checked_add(adjustment).ok_or_else(|| {
        parallel_copy_input_error(
            "EMIT.PARALLEL_COPY_OFFSET",
            format!("stack slot {slot} overflows after temporary adjustment {adjustment}"),
        )
    })
}

fn parallel_copy_input_error(rule: &'static str, message: impl Into<String>) -> EmitError {
    EmitInputError::new(rule, None, None, None, message).into()
}

#[derive(Clone, Copy)]
pub(super) enum EmittedBranchCondition {
    NonZero,
    Compare(CmpKind),
}

pub(super) fn emit_branch_predicate(
    asm: &mut CodeAssembler,
    predicate: BranchPredicate,
    assignment: &AssignmentMap,
) -> Result<EmittedBranchCondition, IcedError> {
    match predicate {
        BranchPredicate::Compare { lhs, rhs, kind } => {
            asm.cmp(
                preg_to_reg64(resolve(assignment, lhs)),
                preg_to_reg64(resolve(assignment, rhs)),
            )?;
            Ok(EmittedBranchCondition::Compare(kind))
        }
        BranchPredicate::CompareImm { lhs, imm, kind } => {
            let lhs = preg_to_reg64(resolve(assignment, lhs));
            if imm == 0 && matches!(kind, CmpKind::Eq | CmpKind::Ne) {
                asm.test(lhs, lhs)?;
            } else {
                asm.cmp(lhs, imm)?;
            }
            Ok(EmittedBranchCondition::Compare(kind))
        }
        BranchPredicate::MemoryNonZero { base, offset, size } => {
            let memory = mem_operand(base, offset);
            match size {
                OpSize::S8 => asm.cmp(byte_ptr(memory), 0)?,
                OpSize::S16 => asm.cmp(word_ptr(memory), 0)?,
                OpSize::S32 => asm.cmp(dword_ptr(memory), 0)?,
                OpSize::S64 => asm.cmp(qword_ptr(memory), 0)?,
            }
            Ok(EmittedBranchCondition::NonZero)
        }
    }
}

fn emit_condition_jump(
    asm: &mut CodeAssembler,
    label: CodeLabel,
    condition: EmittedBranchCondition,
    jump_when_true: bool,
) -> Result<(), IcedError> {
    match (condition, jump_when_true) {
        (EmittedBranchCondition::NonZero, true) => asm.jne(label),
        (EmittedBranchCondition::NonZero, false) => asm.je(label),
        (EmittedBranchCondition::Compare(kind), true) => emit_jcc(asm, label, kind),
        (EmittedBranchCondition::Compare(kind), false) => emit_inverse_jcc(asm, label, kind),
    }
}

pub(super) struct BlockLabels {
    labels: Vec<CodeLabel>,
    canonical: HashMap<BlockId, usize>,
    bound: Vec<bool>,
}

impl BlockLabels {
    pub(super) fn new(
        asm: &mut CodeAssembler,
        func: &MFunction,
        assignment: &AssignmentMap,
        plan: &SsaDestructionPlan,
        block_order: &[usize],
    ) -> Self {
        let mut labels = Vec::new();
        let mut canonical = HashMap::default();

        for (position, &block_index) in block_order.iter().enumerate().rev() {
            let block = &func.blocks[block_index];
            let next = block_order
                .get(position + 1)
                .map(|&next_index| func.blocks[next_index].id);
            let canonical_index = next
                .filter(|&next| block_is_empty_fallthrough(block, next, assignment, plan))
                .and_then(|next| canonical.get(&next).copied())
                .unwrap_or_else(|| {
                    let index = labels.len();
                    labels.push(asm.create_label());
                    index
                });
            canonical.insert(block.id, canonical_index);
        }

        let bound = vec![false; labels.len()];
        Self {
            labels,
            canonical,
            bound,
        }
    }

    pub(super) fn index(&self, block: BlockId) -> Result<usize, EmitError> {
        self.canonical.get(&block).copied().ok_or_else(|| {
            EmitInputError::new(
                "EMIT.BRANCH_TARGET",
                None,
                None,
                None,
                format!("branch targets missing block {block}"),
            )
            .into()
        })
    }

    pub(super) fn label(&self, block: BlockId) -> Result<CodeLabel, EmitError> {
        Ok(self.labels[self.index(block)?])
    }

    pub(super) fn label_mut(&mut self, index: usize) -> &mut CodeLabel {
        &mut self.labels[index]
    }

    pub(super) fn bind(
        &mut self,
        asm: &mut CodeAssembler,
        block: BlockId,
        index: usize,
    ) -> Result<(), EmitError> {
        if self.bound[index] {
            return Ok(());
        }
        asm.set_label(&mut self.labels[index]).map_err(|error| {
            EmitInputError::new(
                "EMIT.BLOCK_LABEL",
                Some(block),
                None,
                None,
                format!("failed to bind native block label: {error}"),
            )
        })?;
        self.bound[index] = true;
        Ok(())
    }

    pub(super) fn mark_bound(&mut self, index: usize) {
        self.bound[index] = true;
    }
}

pub(super) fn instruction_emits_no_code(inst: &MInst, assignment: &AssignmentMap) -> bool {
    match inst {
        MInst::X86Simd(_) => false,
        MInst::Mov { dst, src } => {
            matches!((assignment.get(*dst), assignment.get(*src)), (Some(dst), Some(src)) if dst == src)
        }
        MInst::AndImm {
            dst,
            src,
            imm: u64::MAX,
        }
        | MInst::OrImm { dst, src, imm: 0 } => {
            matches!((assignment.get(*dst), assignment.get(*src)), (Some(dst), Some(src)) if dst == src)
        }
        MInst::CmpSelect {
            dst,
            true_val,
            false_val,
            ..
        }
        | MInst::CmpImmSelect {
            dst,
            true_val,
            false_val,
            ..
        } => matches!(
            (
                assignment.get(*dst),
                assignment.get(*true_val),
                assignment.get(*false_val),
            ),
            (Some(dst), Some(true_val), Some(false_val))
                if dst == true_val && dst == false_val
        ),
        MInst::GuardedCmpSelect {
            dst,
            guard,
            lhs,
            rhs,
            true_val,
            false_val,
            ..
        } => matches!(
            (
                assignment.get(*dst),
                assignment.get(*guard),
                assignment.get(*lhs),
                assignment.get(*rhs),
                assignment.get(*true_val),
                assignment.get(*false_val),
            ),
            (Some(dst), Some(guard), Some(lhs), Some(rhs), Some(true_val), Some(false_val))
                if dst != guard
                    && dst != lhs
                    && dst != rhs
                    && dst == true_val
                    && dst == false_val
        ),
        MInst::MemCopy { byte_len: 0, .. } | MInst::MemFill { byte_len: 0, .. } => true,
        MInst::Scratch { .. }
        | MInst::SparseCommit {
            summary_word_count: 0,
            ..
        }
        | MInst::SparseCommitWorklist {
            active_capacity: 0, ..
        } => true,
        _ => false,
    }
}

fn block_is_empty_fallthrough(
    block: &MBlock,
    next: BlockId,
    assignment: &AssignmentMap,
    plan: &SsaDestructionPlan,
) -> bool {
    matches!(block.terminator(), Some(MInst::Jump { target }) if *target == next)
        && !plan
            .edge(block.id, next)
            .is_some_and(|edge| edge.has_effective_copies())
        && block.insts[..block.insts.len() - 1]
            .iter()
            .all(|inst| instruction_emits_no_code(inst, assignment))
}

/// Choose physical block order after allocation without changing MIR or its
/// SSA edge identities. RPO deliberately places a backedge-only successor
/// late: DFS finishes that edge before walking the loop exit and reversing
/// postorder moves it behind the complete exit region. That is useful for
/// forward allocation, but disastrous when a dedicated CSSA/spill edge block
/// executes on every loop iteration.
///
/// Pull only a linear, single-predecessor, phi-free chain which eventually
/// jumps to a block dominating the branch predecessor. Every selected chain
/// is disjoint because its first and subsequent blocks each have exactly one
/// predecessor. The layout walk itself is `O(B + E)` after the shared forward
/// CFG analysis; that analysis additionally owns its dominance-frontier and
/// natural-loop membership costs. No instruction-sized or pairwise value
/// structure is built here.
pub(super) fn emission_block_order(func: &MFunction) -> Vec<usize> {
    let identity = (0..func.blocks.len()).collect::<Vec<_>>();
    if func.blocks.len() < 2 {
        return identity;
    }

    let block_index = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .filter_map(|successor| block_index.get(&successor).copied())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let Ok(cfg) = ForwardControlFlowGraph::analyze(successors, 0) else {
        // Input verification reports malformed or unreachable CFGs. Layout is
        // only an optimization, so retain the supplied order on raw inputs.
        return identity;
    };

    fn backedge_chain(
        func: &MFunction,
        block_index: &HashMap<BlockId, usize>,
        cfg: &ForwardControlFlowGraph,
        predecessor: usize,
        successor: BlockId,
    ) -> Option<Vec<usize>> {
        let mut expected_predecessor = predecessor;
        let mut current = *block_index.get(&successor)?;
        let mut chain = Vec::new();

        while chain.len() < func.blocks.len() {
            // Only pull blocks currently after the latch. This preserves the
            // existing forward layout and makes every chosen chain unplaced
            // when the latch is visited during the final linear walk.
            if current <= predecessor
                || cfg.predecessors.get(current)?.as_slice() != [expected_predecessor]
                || !func.blocks[current].phis.is_empty()
            {
                return None;
            }
            let MInst::Jump { target } = func.blocks[current].terminator()? else {
                return None;
            };
            let target = *block_index.get(target)?;
            chain.push(current);
            if cfg.dominators.dominates(target, predecessor) {
                return Some(chain);
            }
            expected_predecessor = current;
            current = target;
        }
        None
    }

    let mut claimed = vec![false; func.blocks.len()];
    let mut after = vec![Vec::<usize>::new(); func.blocks.len()];
    for (predecessor, block) in func.blocks.iter().enumerate() {
        let Some((true_bb, false_bb)) = block.terminator().and_then(MInst::branch_targets) else {
            continue;
        };
        for successor in [true_bb, false_bb] {
            let Some(chain) = backedge_chain(func, &block_index, &cfg, predecessor, successor)
            else {
                continue;
            };
            if chain.iter().any(|&index| claimed[index]) {
                continue;
            }
            for &index in &chain {
                claimed[index] = true;
            }
            after[predecessor] = chain;
            break;
        }
    }

    let mut placed = vec![false; func.blocks.len()];
    let mut order = Vec::with_capacity(func.blocks.len());
    for block in 0..func.blocks.len() {
        if placed[block] {
            continue;
        }
        placed[block] = true;
        order.push(block);
        for &edge_block in &after[block] {
            debug_assert!(!placed[edge_block]);
            placed[edge_block] = true;
            order.push(edge_block);
        }
    }
    debug_assert_eq!(order.len(), func.blocks.len());
    order
}

pub(super) fn branch_label(labels: &BlockLabels, block: BlockId) -> Result<CodeLabel, EmitError> {
    labels.label(block).map_err(|_| {
        EmitInputError::new(
            "EMIT.BRANCH_TARGET",
            None,
            None,
            None,
            format!("branch targets missing block {block}"),
        )
        .into()
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_branch_with_edge_copies(
    asm: &mut CodeAssembler,
    labels: &BlockLabels,
    plan: &SsaDestructionPlan,
    predecessor: BlockId,
    true_block: BlockId,
    false_block: BlockId,
    next_block: Option<BlockId>,
    condition: EmittedBranchCondition,
) -> Result<(), EmitError> {
    let true_edge = plan
        .edge(predecessor, true_block)
        .filter(|edge| edge.has_effective_copies());
    let false_edge = plan
        .edge(predecessor, false_block)
        .filter(|edge| edge.has_effective_copies());
    let true_label = branch_label(labels, true_block)?;
    let false_label = branch_label(labels, false_block)?;

    match (true_edge, false_edge) {
        (None, None) => {
            if next_block == Some(true_block) {
                // Invert the branch so the physical true successor is a real
                // fallthrough instead of a taken jump to the next instruction
                // followed by an unconditional false-edge jump.
                emit_condition_jump(asm, false_label, condition, false)?;
            } else {
                emit_condition_jump(asm, true_label, condition, true)?;
            }
            if next_block != Some(false_block) && next_block != Some(true_block) {
                asm.jmp(false_label)?;
            }
        }
        (Some(true_edge), None) => {
            // The false edge can jump directly to its target.  The true edge
            // falls through its copy sequence, avoiding an extra local stub.
            emit_condition_jump(asm, false_label, condition, false)?;
            emit_parallel_copy_plan(asm, Some(true_edge))?;
            if next_block != Some(true_block) {
                asm.jmp(true_label)?;
            }
        }
        (None, Some(false_edge)) => {
            emit_condition_jump(asm, true_label, condition, true)?;
            emit_parallel_copy_plan(asm, Some(false_edge))?;
            if next_block != Some(false_block) {
                asm.jmp(false_label)?;
            }
        }
        (Some(true_edge), Some(false_edge)) if next_block == Some(false_block) => {
            // Place the layout-successor copy last so it can fall through.
            let mut false_copy_label = asm.create_label();
            emit_condition_jump(asm, false_copy_label, condition, false)?;
            emit_parallel_copy_plan(asm, Some(true_edge))?;
            asm.jmp(true_label)?;
            asm.set_label(&mut false_copy_label)?;
            emit_parallel_copy_plan(asm, Some(false_edge))?;
        }
        (Some(true_edge), Some(false_edge)) => {
            let mut true_copy_label = asm.create_label();
            emit_condition_jump(asm, true_copy_label, condition, true)?;
            emit_parallel_copy_plan(asm, Some(false_edge))?;
            asm.jmp(false_label)?;
            asm.set_label(&mut true_copy_label)?;
            emit_parallel_copy_plan(asm, Some(true_edge))?;
            if next_block != Some(true_block) {
                asm.jmp(true_label)?;
            }
        }
    }
    Ok(())
}
