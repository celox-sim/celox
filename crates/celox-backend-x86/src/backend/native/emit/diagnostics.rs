//! MIR and SIR statistics and diagnostic context logging.

use super::*;

pub(super) fn mir_inst_count(func: &crate::native::mir::MFunction) -> usize {
    func.blocks
        .iter()
        .map(|block| block.phis.len() + block.insts.len())
        .sum()
}

pub(super) fn log_mir_stats(label: &str, stage: &str, func: &crate::native::mir::MFunction) {
    let mut phi = 0usize;
    let mut mov = 0usize;
    let mut imm = 0usize;
    let mut load_sim = 0usize;
    let mut load_stack = 0usize;
    let mut load_ptr = 0usize;
    let mut store_sim = 0usize;
    let mut store_stack = 0usize;
    let mut store_ptr = 0usize;
    let mut indexed_load = 0usize;
    let mut indexed_store = 0usize;
    let mut memcopy = 0usize;
    let mut alu = 0usize;
    let mut alu_imm = 0usize;
    let mut cmp = 0usize;
    let mut div_rem = 0usize;
    let mut bit_ops = 0usize;
    let mut select = 0usize;
    let mut branch = 0usize;
    let mut jump = 0usize;
    let mut ret = 0usize;

    for block in &func.blocks {
        phi += block.phis.len();
        for inst in &block.insts {
            match inst {
                MInst::X86Simd(X86SimdInst::Zero128 { .. })
                | MInst::X86Simd(X86SimdInst::Pack128 { .. })
                | MInst::X86Simd(X86SimdInst::Binary128 { .. }) => alu += 1,
                MInst::X86Simd(X86SimdInst::Scratch128 { .. }) => {}
                MInst::X86Simd(X86SimdInst::Load128 { base, .. }) => match base {
                    BaseReg::SimState => load_sim += 1,
                    BaseReg::StackFrame => load_stack += 1,
                },
                MInst::X86Simd(X86SimdInst::Store128 { base, .. }) => match base {
                    BaseReg::SimState => store_sim += 1,
                    BaseReg::StackFrame => store_stack += 1,
                },
                MInst::Mov { .. } | MInst::Mov32 { .. } => mov += 1,
                MInst::LoadImm { .. } | MInst::LoadConstantTableAddr { .. } => imm += 1,
                MInst::Scratch { .. } => {}
                MInst::Load { base, .. } => match base {
                    BaseReg::SimState => load_sim += 1,
                    BaseReg::StackFrame => load_stack += 1,
                },
                MInst::Store { base, .. } => match base {
                    BaseReg::SimState => store_sim += 1,
                    BaseReg::StackFrame => store_stack += 1,
                },
                MInst::AndStoreImm { base, .. } | MInst::OrStoreImm { base, .. } => match base {
                    BaseReg::SimState => {
                        load_sim += 1;
                        store_sim += 1;
                    }
                    BaseReg::StackFrame => {
                        load_stack += 1;
                        store_stack += 1;
                    }
                },
                MInst::LoadPtr { .. } => load_ptr += 1,
                MInst::StorePtr { .. } | MInst::ReleaseStorePtr { .. } => store_ptr += 1,
                MInst::LoadIndexed { .. }
                | MInst::LoadPtrIndexed { .. }
                | MInst::PackedLaneCompare { .. } => indexed_load += 1,
                MInst::StoreIndexed { .. }
                | MInst::OrStoreIndexed { .. }
                | MInst::StorePtrIndexed { .. }
                | MInst::ReleaseStorePtrIndexed { .. } => indexed_store += 1,
                MInst::MemCopy { .. }
                | MInst::MemFill { .. }
                | MInst::SparseCommit { .. }
                | MInst::SparseMarkActive { .. }
                | MInst::SparseCommitWorklist { .. } => memcopy += 1,
                MInst::ExternArg { .. } | MInst::CallExtern { .. } | MInst::ExternResult { .. } => {
                }
                MInst::Add { .. }
                | MInst::Add32 { .. }
                | MInst::Sub { .. }
                | MInst::Sub32 { .. }
                | MInst::Mul { .. }
                | MInst::Mul32 { .. }
                | MInst::UMulHi { .. }
                | MInst::And { .. }
                | MInst::And32 { .. }
                | MInst::Or { .. }
                | MInst::Or32 { .. }
                | MInst::Xor { .. }
                | MInst::Xor32 { .. }
                | MInst::Shr { .. }
                | MInst::Shl { .. }
                | MInst::Sar { .. } => alu += 1,
                MInst::AndImm { .. }
                | MInst::AndImm32 { .. }
                | MInst::MulImm { .. }
                | MInst::MulImm32 { .. }
                | MInst::OrImm { .. }
                | MInst::ShrImm { .. }
                | MInst::ShlImm { .. }
                | MInst::SarImm { .. }
                | MInst::AddImm { .. }
                | MInst::SubImm { .. } => alu_imm += 1,
                MInst::Cmp { .. }
                | MInst::CmpImm { .. }
                | MInst::PackedByteAffineCompare { .. } => cmp += 1,
                MInst::UDiv { .. }
                | MInst::URem { .. }
                | MInst::SDiv { .. }
                | MInst::SRem { .. } => div_rem += 1,
                MInst::BitNot { .. }
                | MInst::Neg { .. }
                | MInst::Popcnt { .. }
                | MInst::Bsf { .. }
                | MInst::Bsr { .. }
                | MInst::BsrOr { .. }
                | MInst::Pext { .. }
                | MInst::Pdep { .. } => bit_ops += 1,
                MInst::Select { .. }
                | MInst::CmpSelect { .. }
                | MInst::CmpImmSelect { .. }
                | MInst::GuardedCmpSelect { .. } => select += 1,
                MInst::Branch { .. } => branch += 1,
                MInst::BranchPred { predicate, .. } => {
                    branch += 1;
                    match predicate {
                        BranchPredicate::Compare { .. } | BranchPredicate::CompareImm { .. } => {
                            cmp += 1;
                        }
                        BranchPredicate::MemoryNonZero { base, .. } => match base {
                            BaseReg::SimState => load_sim += 1,
                            BaseReg::StackFrame => load_stack += 1,
                        },
                    }
                }
                MInst::JumpTable { .. } => jump += 1,
                MInst::Jump { .. } => jump += 1,
                MInst::Return | MInst::ReturnError { .. } => ret += 1,
            }
        }
    }

    tracing::debug!(
        "[native-mir-stats] label={label} stage={stage} phi={phi} mov={mov} imm={imm} load_sim={load_sim} load_stack={load_stack} load_ptr={load_ptr} store_sim={store_sim} store_stack={store_stack} store_ptr={store_ptr} indexed_load={indexed_load} indexed_store={indexed_store} memcopy={memcopy} alu={alu} alu_imm={alu_imm} cmp={cmp} div_rem={div_rem} bit_ops={bit_ops} select={select} branch={branch} jump={jump} ret={ret}"
    );
}

pub(super) fn log_mir_block_stats(label: &str, stage: &str, func: &crate::native::mir::MFunction) {
    let mut blocks = func
        .blocks
        .iter()
        .map(|block| {
            let insts = block.phis.len() + block.insts.len();
            let mut load_sim = 0usize;
            let mut load_stack = 0usize;
            let mut store_sim = 0usize;
            let mut store_stack = 0usize;
            let mut indexed_mem = 0usize;
            let mut memcopy = 0usize;
            let mut imm = 0usize;
            let mut alu = 0usize;
            let mut alu_imm = 0usize;
            let mut cmp = 0usize;
            let mut bit_ops = 0usize;
            let mut select = 0usize;
            let mut control = 0usize;
            for inst in &block.insts {
                match inst {
                    MInst::Load { base, .. } => match base {
                        BaseReg::SimState => load_sim += 1,
                        BaseReg::StackFrame => load_stack += 1,
                    },
                    MInst::Store { base, .. } => match base {
                        BaseReg::SimState => store_sim += 1,
                        BaseReg::StackFrame => store_stack += 1,
                    },
                    MInst::LoadIndexed { .. }
                    | MInst::LoadPtrIndexed { .. }
                    | MInst::StoreIndexed { .. }
                    | MInst::OrStoreIndexed { .. }
                    | MInst::StorePtrIndexed { .. }
                    | MInst::ReleaseStorePtrIndexed { .. } => indexed_mem += 1,
                    MInst::MemCopy { .. } | MInst::MemFill { .. } => memcopy += 1,
                    MInst::LoadImm { .. } | MInst::LoadConstantTableAddr { .. } => imm += 1,
                    MInst::Add { .. }
                    | MInst::Add32 { .. }
                    | MInst::Sub { .. }
                    | MInst::Sub32 { .. }
                    | MInst::Mul { .. }
                    | MInst::Mul32 { .. }
                    | MInst::UMulHi { .. }
                    | MInst::And { .. }
                    | MInst::And32 { .. }
                    | MInst::Or { .. }
                    | MInst::Or32 { .. }
                    | MInst::Xor { .. }
                    | MInst::Xor32 { .. }
                    | MInst::Shr { .. }
                    | MInst::Shl { .. }
                    | MInst::Sar { .. } => alu += 1,
                    MInst::AndImm { .. }
                    | MInst::AndImm32 { .. }
                    | MInst::MulImm { .. }
                    | MInst::MulImm32 { .. }
                    | MInst::OrImm { .. }
                    | MInst::ShrImm { .. }
                    | MInst::ShlImm { .. }
                    | MInst::SarImm { .. }
                    | MInst::AddImm { .. }
                    | MInst::SubImm { .. } => alu_imm += 1,
                    MInst::Cmp { .. } | MInst::CmpImm { .. } => cmp += 1,
                    MInst::BitNot { .. }
                    | MInst::Neg { .. }
                    | MInst::Popcnt { .. }
                    | MInst::Bsf { .. }
                    | MInst::Bsr { .. }
                    | MInst::BsrOr { .. }
                    | MInst::Pext { .. }
                    | MInst::Pdep { .. } => bit_ops += 1,
                    MInst::Select { .. }
                    | MInst::CmpSelect { .. }
                    | MInst::CmpImmSelect { .. }
                    | MInst::GuardedCmpSelect { .. } => select += 1,
                    MInst::Branch { .. }
                    | MInst::BranchPred { .. }
                    | MInst::Jump { .. }
                    | MInst::Return
                    | MInst::ReturnError { .. } => control += 1,
                    _ => {}
                }
            }
            (
                insts,
                block.id.0,
                block.phis.len(),
                block.insts.len(),
                load_sim,
                load_stack,
                store_sim,
                store_stack,
                indexed_mem,
                memcopy,
                imm,
                alu,
                alu_imm,
                cmp,
                bit_ops,
                select,
                control,
            )
        })
        .collect::<Vec<_>>();
    blocks.sort_unstable_by_key(|entry| (std::cmp::Reverse(entry.0), entry.1));
    for (
        rank,
        (
            total,
            block_id,
            phis,
            insts,
            load_sim,
            load_stack,
            store_sim,
            store_stack,
            indexed_mem,
            memcopy,
            imm,
            alu,
            alu_imm,
            cmp,
            bit_ops,
            select,
            control,
        ),
    ) in blocks.into_iter().take(10).enumerate()
    {
        tracing::debug!(
            "[native-mir-block-stats] label={label} stage={stage} rank={} block={} total={} phis={} insts={} load_sim={} load_stack={} store_sim={} store_stack={} indexed_mem={} memcopy={} imm={} alu={} alu_imm={} cmp={} bit_ops={} select={} control={}",
            rank + 1,
            block_id,
            total,
            phis,
            insts,
            load_sim,
            load_stack,
            store_sim,
            store_stack,
            indexed_mem,
            memcopy,
            imm,
            alu,
            alu_imm,
            cmp,
            bit_ops,
            select,
            control
        );
    }
}

pub(super) fn dump_native_block_context(
    label: &str,
    stage: &str,
    eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    func: &crate::native::mir::MFunction,
    diagnostics: &crate::NativeDiagnostics,
) {
    let Some(options) = diagnostics.dump.as_ref() else {
        return;
    };
    if let Some(configured_label) = options.label.as_deref()
        && configured_label != label
    {
        return;
    }
    if let Some(configured_stage) = options.stage.as_deref() {
        if configured_stage != stage {
            return;
        }
    } else if stage != "after_isel" {
        return;
    }
    let Ok(block_id) = u32::try_from(options.block) else {
        return;
    };
    let sir_id = crate::BlockId(block_id as usize);
    tracing::debug!("[native-dump] label={label} stage={stage} block={block_id}");
    if options.dump_sir {
        if let Some(block) = eu.blocks.get(&sir_id) {
            tracing::debug!("[native-dump] SIR:\n{block}");
            dump_sir_operand_defs(eu, block);
        } else {
            tracing::debug!("[native-dump] SIR block b{block_id} not found");
        }
    }
    if let Some(block) = func
        .blocks
        .iter()
        .find(|block| block.id == crate::native::mir::BlockId(block_id))
    {
        tracing::debug!(
            "[native-dump] MIR b{} phis={} insts={}",
            block.id.0,
            block.phis.len(),
            block.insts.len()
        );
        for phi in &block.phis {
            let sources = phi
                .sources
                .iter()
                .map(|(pred, src)| format!("b{}:{}", pred.0, src))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::debug!("  {} = phi({sources})", phi.dst);
        }
        for (idx, inst) in block.insts.iter().enumerate().take(options.mir_limit) {
            tracing::debug!("  {idx}: {inst}");
        }
        if block.insts.len() > options.mir_limit {
            tracing::debug!("  ... {} more insts", block.insts.len() - options.mir_limit);
        }
    } else {
        tracing::debug!("[native-dump] MIR block b{block_id} not found");
    }
}

fn dump_sir_operand_defs(
    eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>,
    block: &crate::BasicBlock<crate::RegionedAbsoluteAddr>,
) {
    let mut regs = Vec::new();
    for inst in &block.instructions {
        collect_sir_inst_uses(inst, &mut regs);
    }
    regs.sort();
    regs.dedup();
    for reg in regs {
        let mut found = false;
        for other in eu.blocks.values() {
            if other.params.contains(&reg) {
                tracing::debug!("  [sir-def] r{} is param of b{}", reg.0, other.id.0);
                found = true;
            }
            for (idx, inst) in other.instructions.iter().enumerate() {
                if sir_inst_def(inst) == Some(reg) {
                    tracing::debug!(
                        "  [sir-def] r{} defined at b{} inst {}: {}",
                        reg.0,
                        other.id.0,
                        idx,
                        inst
                    );
                    found = true;
                }
            }
        }
        if !found {
            tracing::debug!("  [sir-def] r{} has no SIR definition", reg.0);
        }
    }
}

fn sir_inst_def(
    inst: &crate::SIRInstruction<crate::RegionedAbsoluteAddr>,
) -> Option<crate::RegisterId> {
    inst.defined_register()
}

fn collect_sir_inst_uses(
    inst: &crate::SIRInstruction<crate::RegionedAbsoluteAddr>,
    out: &mut Vec<crate::RegisterId>,
) {
    inst.for_each_use(|register| out.push(register));
}

pub(super) fn log_sir_width_stats(eu: &crate::ExecutionUnit<crate::RegionedAbsoluteAddr>) {
    use crate::{RegisterType, SIRInstruction};

    let mut max_reg_width = 0usize;
    let mut regs_gt_1024 = 0usize;
    for reg_ty in eu.register_map.values() {
        let width = match reg_ty {
            RegisterType::Logic { width } | RegisterType::Bit { width, .. } => *width,
        };
        max_reg_width = max_reg_width.max(width);
        if width > 1024 {
            regs_gt_1024 += 1;
        }
    }

    let mut max_inst_width = 0usize;
    let mut wide_loads = 0usize;
    let mut wide_stores = 0usize;
    let mut wide_commits = 0usize;
    let mut wide_slices = 0usize;
    let mut est_chunks = 0usize;
    let mut examples = Vec::new();
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Load(_, addr, offset, width) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_loads += 1;
                        if examples.len() < 8 {
                            examples.push(format!(
                                "Load addr={addr:?} offset={offset:?} width={width}"
                            ));
                        }
                    }
                }
                SIRInstruction::Store(addr, offset, width, _, _, _) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_stores += 1;
                        if examples.len() < 8 {
                            examples.push(format!(
                                "Store addr={addr:?} offset={offset:?} width={width}"
                            ));
                        }
                    }
                }
                SIRInstruction::Commit(src, dst, offset, width, _) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_commits += 1;
                        if examples.len() < 8 {
                            examples.push(format!(
                                "Commit src={src:?} dst={dst:?} offset={offset:?} width={width}"
                            ));
                        }
                    }
                }
                SIRInstruction::Slice(_, _, offset, width) => {
                    max_inst_width = max_inst_width.max(*width);
                    est_chunks += width.div_ceil(64);
                    if *width > 1024 {
                        wide_slices += 1;
                        if examples.len() < 8 {
                            examples.push(format!("Slice offset={offset} width={width}"));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    tracing::debug!(
        "[native-timing] sir_width_stats regs={} regs_gt_1024={} max_reg_width={} max_inst_width={} wide_loads={} wide_stores={} wide_commits={} wide_slices={} est_width_chunks={}",
        eu.register_map.len(),
        regs_gt_1024,
        max_reg_width,
        max_inst_width,
        wide_loads,
        wide_stores,
        wide_commits,
        wide_slices,
        est_chunks
    );
    for example in examples {
        tracing::debug!("[native-timing] sir_width_example {example}");
    }
}
