use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use num_bigint::BigUint;
use num_traits::Zero;

use super::{
    BasicBlock, BinaryOp, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction,
    SIROffset, SIRTerminator, UnaryOp,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SirVerifyError {
    pub invariant: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub message: String,
}

impl SirVerifyError {
    fn function(invariant: &'static str, message: impl Into<String>) -> Self {
        Self {
            invariant,
            block: None,
            instruction: None,
            message: message.into(),
        }
    }

    fn block(invariant: &'static str, block: BlockId, message: impl Into<String>) -> Self {
        Self {
            invariant,
            block: Some(block),
            instruction: None,
            message: message.into(),
        }
    }

    pub(crate) fn instruction(
        invariant: &'static str,
        block: BlockId,
        instruction: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            invariant,
            block: Some(block),
            instruction: Some(instruction),
            message: message.into(),
        }
    }
}

impl fmt::Display for SirVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SIR verify [{}]", self.invariant)?;
        if let Some(block) = self.block {
            write!(f, " at b{}", block.0)?;
        }
        if let Some(instruction) = self.instruction {
            write!(f, "/i{instruction}")?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for SirVerifyError {}

#[derive(Clone, Copy)]
enum DefSite {
    Param(BlockId),
    Inst(BlockId, usize),
}

impl<A> ExecutionUnit<A> {
    /// Verify the canonical SIR contract without modifying the IR.
    pub fn verify_result(&self) -> Result<(), SirVerifyError> {
        verify_execution_unit(self)
    }

    /// Verify the canonical SIR contract and panic with a structured diagnostic.
    pub fn verify(&self) {
        if let Err(error) = self.verify_result() {
            panic!("{error}");
        }
    }
}

fn verify_execution_unit<A>(eu: &ExecutionUnit<A>) -> Result<(), SirVerifyError> {
    if eu.blocks.is_empty() {
        return Err(SirVerifyError::function(
            "CFG.NON_EMPTY",
            "execution unit has no blocks",
        ));
    }
    if !eu.blocks.contains_key(&eu.entry_block_id) {
        return Err(SirVerifyError::function(
            "CFG.ENTRY_EXISTS",
            format!("entry block b{} does not exist", eu.entry_block_id.0),
        ));
    }
    for (&reg, ty) in &eu.register_map {
        if ty.width() == 0 {
            return Err(SirVerifyError::function(
                "TYPE.NON_ZERO_WIDTH",
                format!("r{} has zero width", reg.0),
            ));
        }
    }

    let mut predecessors: BTreeMap<BlockId, BTreeSet<BlockId>> = eu
        .blocks
        .keys()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect();

    for (&key, block) in &eu.blocks {
        if key != block.id {
            return Err(SirVerifyError::block(
                "CFG.BLOCK_KEY_MATCHES_ID",
                key,
                format!("map key b{} contains block with id b{}", key.0, block.id.0),
            ));
        }
        for target in successor_ids(&block.terminator) {
            let Some(preds) = predecessors.get_mut(&target) else {
                return Err(SirVerifyError::block(
                    "CFG.TARGET_EXISTS",
                    block.id,
                    format!("terminator targets missing block b{}", target.0),
                ));
            };
            preds.insert(block.id);
        }
        verify_edges(eu, block)?;
    }

    let reachable = reachable_blocks(eu);
    if reachable.len() != eu.blocks.len() {
        let unreachable = eu
            .blocks
            .keys()
            .filter(|id| !reachable.contains(id))
            .map(|id| format!("b{}", id.0))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(SirVerifyError::function(
            "CFG.ALL_BLOCKS_REACHABLE",
            format!("unreachable blocks: {unreachable}"),
        ));
    }

    let mut defs = BTreeMap::new();
    for block in eu.blocks.values() {
        let mut params = BTreeSet::new();
        for &param in &block.params {
            require_register(eu, block.id, None, param)?;
            if !params.insert(param) {
                return Err(SirVerifyError::block(
                    "SSA.UNIQUE_BLOCK_PARAMS",
                    block.id,
                    format!("r{} occurs more than once in block parameters", param.0),
                ));
            }
            insert_def(&mut defs, param, DefSite::Param(block.id), block.id, None)?;
        }
        for (index, inst) in block.instructions.iter().enumerate() {
            verify_instruction_types(eu, block.id, index, inst)?;
            if let Some(dst) = instruction_def(inst) {
                require_register(eu, block.id, Some(index), dst)?;
                insert_def(
                    &mut defs,
                    dst,
                    DefSite::Inst(block.id, index),
                    block.id,
                    Some(index),
                )?;
            }
        }
    }

    let dominators = compute_dominators(eu, &predecessors, &reachable);
    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            for reg in instruction_uses(inst) {
                verify_use(eu, &defs, &dominators, block.id, index, reg)?;
            }
        }
        let term_index = block.instructions.len();
        for reg in terminator_uses(&block.terminator) {
            verify_use(eu, &defs, &dominators, block.id, term_index, reg)?;
        }
    }

    Ok(())
}

fn verify_edges<A>(eu: &ExecutionUnit<A>, block: &BasicBlock<A>) -> Result<(), SirVerifyError> {
    let edges: Vec<(BlockId, &[RegisterId])> = match &block.terminator {
        SIRTerminator::Jump(target, args) => vec![(*target, args)],
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let cond_ty = register_type(eu, block.id, None, *cond)?;
            if !matches!(
                cond_ty,
                RegisterType::Bit {
                    width: 1,
                    signed: false
                }
            ) {
                return Err(SirVerifyError::block(
                    "TYPE.BRANCH_CONDITION",
                    block.id,
                    format!(
                        "branch condition r{} has type {cond_ty:?}, expected unsigned bit<1>",
                        cond.0
                    ),
                ));
            }
            vec![
                (true_block.0, true_block.1.as_slice()),
                (false_block.0, false_block.1.as_slice()),
            ]
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    };

    for (target, args) in edges {
        let Some(target_block) = eu.blocks.get(&target) else {
            continue;
        };
        if args.len() != target_block.params.len() {
            return Err(SirVerifyError::block(
                "CFG.EDGE_ARITY",
                block.id,
                format!(
                    "edge to b{} passes {} arguments, expected {}",
                    target.0,
                    args.len(),
                    target_block.params.len()
                ),
            ));
        }
        for (position, (&arg, &param)) in args.iter().zip(&target_block.params).enumerate() {
            let arg_ty = register_type(eu, block.id, None, arg)?;
            let param_ty = register_type(eu, target, None, param)?;
            if arg_ty.width() != param_ty.width() {
                return Err(SirVerifyError::block(
                    "TYPE.EDGE_ARGUMENT",
                    block.id,
                    format!(
                        "edge argument {position} to b{} has width {}, parameter r{} has width {}",
                        target.0,
                        arg_ty.width(),
                        param.0,
                        param_ty.width()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn verify_instruction_types<A>(
    eu: &ExecutionUnit<A>,
    block: BlockId,
    index: usize,
    inst: &SIRInstruction<A>,
) -> Result<(), SirVerifyError> {
    let ty = |reg| register_type(eu, block, Some(index), reg);
    let same_width = |lhs: RegisterId, rhs: RegisterId, invariant| -> Result<(), SirVerifyError> {
        let lhs_ty = ty(lhs)?;
        let rhs_ty = ty(rhs)?;
        if lhs_ty.width() != rhs_ty.width() {
            return Err(SirVerifyError::instruction(
                invariant,
                block,
                index,
                format!(
                    "r{} has width {}, but r{} has width {}",
                    lhs.0,
                    lhs_ty.width(),
                    rhs.0,
                    rhs_ty.width()
                ),
            ));
        }
        Ok(())
    };
    let exact_width = |reg: RegisterId, expected: usize, invariant| -> Result<(), SirVerifyError> {
        let actual = ty(reg)?.width();
        if actual != expected {
            return Err(SirVerifyError::instruction(
                invariant,
                block,
                index,
                format!("r{} has width {actual}, expected {expected}", reg.0),
            ));
        }
        Ok(())
    };
    let unsigned_if_bit = |reg: RegisterId, invariant| -> Result<(), SirVerifyError> {
        if matches!(ty(reg)?, RegisterType::Bit { signed: true, .. }) {
            return Err(SirVerifyError::instruction(
                invariant,
                block,
                index,
                format!("r{} is a signed Bit result", reg.0),
            ));
        }
        Ok(())
    };

    match inst {
        SIRInstruction::Imm(dst, value) => {
            let dst_ty = ty(*dst)?;
            let width = dst_ty.width();
            if !fits_width(&value.payload, width) || !fits_width(&value.mask, width) {
                return Err(SirVerifyError::instruction(
                    "TYPE.IMMEDIATE_FITS_DESTINATION",
                    block,
                    index,
                    format!("immediate does not fit r{} width {width}", dst.0),
                ));
            }
            if matches!(dst_ty, RegisterType::Bit { .. }) && !value.mask.is_zero() {
                return Err(SirVerifyError::instruction(
                    "TYPE.BIT_IMMEDIATE_MASK",
                    block,
                    index,
                    format!("two-state r{} has a nonzero immediate mask", dst.0),
                ));
            }
        }
        SIRInstruction::Binary(dst, lhs, op, rhs) => match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard => {
                exact_width(*dst, 1, "TYPE.COMPARISON_RESULT_WIDTH")?;
                unsigned_if_bit(*dst, "TYPE.BOOLEAN_RESULT_UNSIGNED")?;
                same_width(*lhs, *rhs, "TYPE.COMPARISON_OPERAND_WIDTH")?;
            }
            BinaryOp::LogicAnd | BinaryOp::LogicOr => {
                exact_width(*dst, 1, "TYPE.LOGICAL_RESULT_WIDTH")?;
                unsigned_if_bit(*dst, "TYPE.BOOLEAN_RESULT_UNSIGNED")?;
                ty(*lhs)?;
                ty(*rhs)?;
            }
            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                same_width(*dst, *lhs, "TYPE.DIVREM_LHS_WIDTH")?;
                same_width(*dst, *rhs, "TYPE.DIVREM_RHS_WIDTH")?;
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
                ty(*dst)?;
                ty(*lhs)?;
                ty(*rhs)?;
            }
            _ => {
                ty(*dst)?;
                ty(*lhs)?;
                ty(*rhs)?;
            }
        },
        SIRInstruction::Unary(dst, op, src) => match op {
            UnaryOp::LogicNot | UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => {
                exact_width(*dst, 1, "TYPE.REDUCTION_RESULT_WIDTH")?;
                unsigned_if_bit(*dst, "TYPE.BOOLEAN_RESULT_UNSIGNED")?;
                ty(*src)?;
            }
            UnaryOp::ToTwoState => {
                same_width(*dst, *src, "TYPE.TWO_STATE_WIDTH")?;
                if !matches!(ty(*dst)?, RegisterType::Bit { .. }) {
                    return Err(SirVerifyError::instruction(
                        "TYPE.TWO_STATE_DESTINATION",
                        block,
                        index,
                        format!("r{} is not a Bit register", dst.0),
                    ));
                }
            }
            UnaryOp::Minus | UnaryOp::BitNot => {
                same_width(*dst, *src, "TYPE.UNARY_VALUE_WIDTH")?;
            }
            UnaryOp::Ident => {
                ty(*dst)?;
                ty(*src)?;
            }
            UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                let dst_width = ty(*dst)?.width();
                let src_width = ty(*src)?.width();
                let required_width = op.result_width(src_width);
                if dst_width < required_width {
                    return Err(SirVerifyError::instruction(
                        "TYPE.BIT_COUNT_RESULT_WIDTH",
                        block,
                        index,
                        format!(
                            "{op} of r{} width {src_width} may produce {src_width}, which requires at least {required_width} result bits, but r{} has width {dst_width}",
                            src.0, dst.0,
                        ),
                    ));
                }
            }
        },
        SIRInstruction::Load(dst, _, offset, bits) => {
            non_zero_width(block, index, *bits, "TYPE.LOAD_NON_ZERO")?;
            if ty(*dst)?.width() != *bits {
                return Err(SirVerifyError::instruction(
                    "TYPE.LOAD_WIDTH",
                    block,
                    index,
                    format!(
                        "load width {bits} does not match r{} width {}",
                        dst.0,
                        ty(*dst)?.width()
                    ),
                ));
            }
            verify_offset(eu, block, index, offset, *bits)?;
        }
        SIRInstruction::Store(_, offset, 0, _, triggers, _) => {
            if triggers.is_empty() || !matches!(offset, SIROffset::Static(_)) {
                return Err(SirVerifyError::instruction(
                    "STORE.TRIGGER_ONLY_FORM",
                    block,
                    index,
                    "zero-width store requires triggers and a static offset",
                ));
            }
        }
        SIRInstruction::Store(_, offset, bits, src, _, _) => {
            if ty(*src)?.width() < *bits {
                return Err(SirVerifyError::instruction(
                    "TYPE.STORE_SOURCE_WIDTH",
                    block,
                    index,
                    format!(
                        "store width {bits} exceeds r{} width {}",
                        src.0,
                        ty(*src)?.width()
                    ),
                ));
            }
            verify_offset(eu, block, index, offset, *bits)?;
        }
        SIRInstruction::Commit(_, _, offset, bits, _) => {
            non_zero_width(block, index, *bits, "TYPE.COMMIT_NON_ZERO")?;
            verify_offset(eu, block, index, offset, *bits)?;
        }
        SIRInstruction::Concat(dst, args) => {
            if args.is_empty() {
                return Err(SirVerifyError::instruction(
                    "TYPE.CONCAT_NON_EMPTY",
                    block,
                    index,
                    "concat has no operands",
                ));
            }
            let mut width = 0usize;
            for &arg in args {
                width = width.checked_add(ty(arg)?.width()).ok_or_else(|| {
                    SirVerifyError::instruction(
                        "TYPE.WIDTH_OVERFLOW",
                        block,
                        index,
                        "concat width overflows usize",
                    )
                })?;
            }
            if ty(*dst)?.width() != width {
                return Err(SirVerifyError::instruction(
                    "TYPE.CONCAT_WIDTH",
                    block,
                    index,
                    format!(
                        "concat operands total {width} bits, r{} has width {}",
                        dst.0,
                        ty(*dst)?.width()
                    ),
                ));
            }
        }
        SIRInstruction::Slice(dst, src, offset, width) => {
            non_zero_width(block, index, *width, "TYPE.SLICE_NON_ZERO")?;
            let end = offset.checked_add(*width).ok_or_else(|| {
                SirVerifyError::instruction(
                    "TYPE.WIDTH_OVERFLOW",
                    block,
                    index,
                    "slice range overflows usize",
                )
            })?;
            if end > ty(*src)?.width() {
                return Err(SirVerifyError::instruction(
                    "TYPE.SLICE_BOUNDS",
                    block,
                    index,
                    format!(
                        "slice [{offset} +: {width}] exceeds r{} width {}",
                        src.0,
                        ty(*src)?.width()
                    ),
                ));
            }
            if ty(*dst)?.width() != *width {
                return Err(SirVerifyError::instruction(
                    "TYPE.SLICE_RESULT_WIDTH",
                    block,
                    index,
                    format!(
                        "slice width {width} does not match r{} width {}",
                        dst.0,
                        ty(*dst)?.width()
                    ),
                ));
            }
        }
        SIRInstruction::Mux(dst, cond, then_value, else_value) => {
            ty(*cond)?;
            same_width(*dst, *then_value, "TYPE.MUX_THEN_WIDTH")?;
            same_width(*dst, *else_value, "TYPE.MUX_ELSE_WIDTH")?;
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for &arg in args {
                ty(arg)?;
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            same_width(*old, *new, "TYPE.CAPTURE_COMPARE_OPERANDS")?;
        }
    }
    Ok(())
}

fn verify_offset<A>(
    eu: &ExecutionUnit<A>,
    block: BlockId,
    index: usize,
    offset: &SIROffset,
    access_width: usize,
) -> Result<(), SirVerifyError> {
    match offset {
        SIROffset::Static(_) => {}
        SIROffset::Dynamic(reg) => {
            register_type(eu, block, Some(index), *reg)?;
        }
        SIROffset::Element {
            index: element_index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => {
            register_type(eu, block, Some(index), *element_index)?;
            if let Some(dynamic_bit_offset) = dynamic_bit_offset {
                register_type(eu, block, Some(index), *dynamic_bit_offset)?;
            }
            if *element_width == 0 {
                return Err(SirVerifyError::instruction(
                    "MEMORY.ELEMENT_WIDTH_NON_ZERO",
                    block,
                    index,
                    "unpacked-array element width is zero",
                ));
            }
            let Some(access_end) = bit_offset.checked_add(access_width) else {
                return Err(SirVerifyError::instruction(
                    "MEMORY.ELEMENT_ACCESS_IN_BOUNDS",
                    block,
                    index,
                    "element access range overflows usize",
                ));
            };
            if access_end > *element_width {
                return Err(SirVerifyError::instruction(
                    "MEMORY.ELEMENT_ACCESS_IN_BOUNDS",
                    block,
                    index,
                    format!(
                        "element access [{bit_offset}..{access_end}) exceeds element width {element_width}"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn non_zero_width(
    block: BlockId,
    index: usize,
    width: usize,
    invariant: &'static str,
) -> Result<(), SirVerifyError> {
    if width == 0 {
        return Err(SirVerifyError::instruction(
            invariant,
            block,
            index,
            "width is zero",
        ));
    }
    Ok(())
}

fn fits_width(value: &BigUint, width: usize) -> bool {
    value.is_zero() || value.bits() <= width as u64
}

fn register_type<A>(
    eu: &ExecutionUnit<A>,
    block: BlockId,
    instruction: Option<usize>,
    reg: RegisterId,
) -> Result<&RegisterType, SirVerifyError> {
    eu.register_map.get(&reg).ok_or_else(|| match instruction {
        Some(index) => SirVerifyError::instruction(
            "REGISTER.DECLARED",
            block,
            index,
            format!("r{} is not present in register_map", reg.0),
        ),
        None => SirVerifyError::block(
            "REGISTER.DECLARED",
            block,
            format!("r{} is not present in register_map", reg.0),
        ),
    })
}

fn require_register<A>(
    eu: &ExecutionUnit<A>,
    block: BlockId,
    instruction: Option<usize>,
    reg: RegisterId,
) -> Result<(), SirVerifyError> {
    register_type(eu, block, instruction, reg).map(|_| ())
}

fn insert_def(
    defs: &mut BTreeMap<RegisterId, DefSite>,
    reg: RegisterId,
    site: DefSite,
    block: BlockId,
    instruction: Option<usize>,
) -> Result<(), SirVerifyError> {
    if defs.insert(reg, site).is_some() {
        return Err(match instruction {
            Some(index) => SirVerifyError::instruction(
                "SSA.SINGLE_DEFINITION",
                block,
                index,
                format!("r{} is defined more than once", reg.0),
            ),
            None => SirVerifyError::block(
                "SSA.SINGLE_DEFINITION",
                block,
                format!("r{} is defined more than once", reg.0),
            ),
        });
    }
    Ok(())
}

fn verify_use<A>(
    eu: &ExecutionUnit<A>,
    defs: &BTreeMap<RegisterId, DefSite>,
    dominators: &Dominators,
    block: BlockId,
    instruction: usize,
    reg: RegisterId,
) -> Result<(), SirVerifyError> {
    require_register(eu, block, Some(instruction), reg)?;
    let Some(site) = defs.get(&reg).copied() else {
        return Err(SirVerifyError::instruction(
            "SSA.USE_HAS_DEFINITION",
            block,
            instruction,
            format!("r{} is used but never defined", reg.0),
        ));
    };
    let valid = match site {
        DefSite::Param(def_block) => dominators.dominates(def_block, block),
        DefSite::Inst(def_block, def_index) => {
            (def_block == block && def_index < instruction)
                || (def_block != block && dominators.dominates(def_block, block))
        }
    };
    if !valid {
        return Err(SirVerifyError::instruction(
            "SSA.DEFINITION_DOMINATES_USE",
            block,
            instruction,
            format!("definition of r{} does not dominate this use", reg.0),
        ));
    }
    Ok(())
}

fn instruction_def<A>(inst: &SIRInstruction<A>) -> Option<RegisterId> {
    match inst {
        SIRInstruction::Imm(dst, _)
        | SIRInstruction::Binary(dst, _, _, _)
        | SIRInstruction::Unary(dst, _, _)
        | SIRInstruction::Load(dst, _, _, _)
        | SIRInstruction::Concat(dst, _)
        | SIRInstruction::Slice(dst, _, _, _)
        | SIRInstruction::Mux(dst, _, _, _) => Some(*dst),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
    }
}

fn instruction_uses<A>(inst: &SIRInstruction<A>) -> Vec<RegisterId> {
    let mut uses = Vec::new();
    match inst {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => uses.extend([*lhs, *rhs]),
        SIRInstruction::Unary(_, _, src) => uses.push(*src),
        SIRInstruction::Load(_, _, offset, _) => {
            uses.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Store(_, offset, bits, src, _, _) => {
            if *bits != 0 {
                uses.extend(offset.dynamic_registers().into_iter().flatten());
                uses.push(*src);
            }
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            uses.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => uses.extend(args.iter().copied()),
        SIRInstruction::Slice(_, src, _, _) => uses.push(*src),
        SIRInstruction::Mux(_, cond, then_value, else_value) => {
            uses.extend([*cond, *then_value, *else_value]);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            uses.extend([*old, *new]);
        }
    }
    uses
}

fn terminator_uses(term: &SIRTerminator) -> Vec<RegisterId> {
    match term {
        SIRTerminator::Jump(_, args) => args.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let mut uses = vec![*cond];
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
            uses
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn successor_ids(term: &SIRTerminator) -> Vec<BlockId> {
    match term {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn reachable_blocks<A>(eu: &ExecutionUnit<A>) -> BTreeSet<BlockId> {
    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::from([eu.entry_block_id]);
    while let Some(block) = queue.pop_front() {
        if !reachable.insert(block) {
            continue;
        }
        for successor in successor_ids(&eu.blocks[&block].terminator) {
            if eu.blocks.contains_key(&successor) {
                queue.push_back(successor);
            }
        }
    }
    reachable
}

fn compute_dominators<A>(
    eu: &ExecutionUnit<A>,
    predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
    reachable: &BTreeSet<BlockId>,
) -> Dominators {
    let mut successors = reachable
        .iter()
        .copied()
        .map(|block| (block, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for (&block, block_predecessors) in predecessors {
        if !reachable.contains(&block) {
            continue;
        }
        for &predecessor in block_predecessors {
            if reachable.contains(&predecessor) {
                successors.get_mut(&predecessor).unwrap().push(block);
            }
        }
    }
    let entry = eu.entry_block_id;
    let mut visited = BTreeSet::new();
    let mut postorder = Vec::with_capacity(reachable.len());
    visited.insert(entry);
    let mut stack = vec![(entry, 0usize)];
    while let Some((block, next)) = stack.last_mut() {
        if *next == successors[block].len() {
            postorder.push(*block);
            stack.pop();
            continue;
        }
        let successor = successors[block][*next];
        *next += 1;
        if visited.insert(successor) {
            stack.push((successor, 0));
        }
    }
    postorder.reverse();
    let index = postorder
        .iter()
        .enumerate()
        .map(|(index, &id)| (id, index))
        .collect::<BTreeMap<_, _>>();
    let mut idom = vec![None; postorder.len()];
    idom[0] = Some(0);
    let intersect = |mut left: usize, mut right: usize, idom: &[Option<usize>]| {
        while left != right {
            while left > right {
                left = idom[left].expect("processed dominator");
            }
            while right > left {
                right = idom[right].expect("processed dominator");
            }
        }
        left
    };
    loop {
        let mut changed = false;
        for block_index in 1..postorder.len() {
            let block = postorder[block_index];
            let mut processed = predecessors[&block]
                .iter()
                .map(|predecessor| index[predecessor])
                .filter(|predecessor| idom[*predecessor].is_some());
            let Some(first) = processed.next() else {
                continue;
            };
            let next = processed.fold(first, |current, predecessor| {
                intersect(current, predecessor, &idom)
            });
            if idom[block_index] != Some(next) {
                idom[block_index] = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut children = vec![Vec::new(); postorder.len()];
    for (block, parent) in idom.iter().enumerate().skip(1) {
        children[parent.expect("reachable block has idom")].push(block);
    }
    let mut enter = vec![0usize; postorder.len()];
    let mut exit = vec![0usize; postorder.len()];
    let mut time = 0usize;
    let mut events = vec![(0usize, false)];
    while let Some((block, leaving)) = events.pop() {
        if leaving {
            exit[block] = time;
            time += 1;
        } else {
            enter[block] = time;
            time += 1;
            events.push((block, true));
            events.extend(children[block].iter().rev().map(|child| (*child, false)));
        }
    }
    Dominators { index, enter, exit }
}

struct Dominators {
    index: BTreeMap<BlockId, usize>,
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl Dominators {
    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let dominator = self.index[&dominator];
        let block = self.index[&block];
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }
}

#[cfg(test)]
mod tests {
    use crate::HashMap;
    use crate::ir::{SIRInstruction, SIRValue};

    use super::*;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn signed_bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: true,
        }
    }

    fn logic(width: usize) -> RegisterType {
        RegisterType::Logic { width }
    }

    fn unit(
        blocks: impl IntoIterator<Item = BasicBlock<usize>>,
        registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
    ) -> ExecutionUnit<usize> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|b| (b.id, b)).collect(),
            register_map: registers.into_iter().collect::<HashMap<_, _>>(),
        }
    }

    #[test]
    fn accepts_well_formed_branch_and_block_parameter() {
        let eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                    ],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![RegisterId(2)],
                    instructions: vec![SIRInstruction::Unary(
                        RegisterId(3),
                        UnaryOp::Ident,
                        RegisterId(2),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );
        assert_eq!(eu.verify_result(), Ok(()));
    }

    #[test]
    fn rejects_use_not_dominated_by_definition() {
        let eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![],
                    instructions: vec![SIRInstruction::Imm(RegisterId(1), SIRValue::new(3u8))],
                    terminator: SIRTerminator::Jump(BlockId(2), vec![]),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: vec![],
                    instructions: vec![SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "SSA.DEFINITION_DOMINATES_USE"
        );
    }

    #[test]
    fn rejects_edge_type_mismatch() {
        let eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                    terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![RegisterId(1)],
                    instructions: vec![],
                    terminator: SIRTerminator::Return,
                },
            ],
            [(RegisterId(0), bit(1)), (RegisterId(1), bit(2))],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.EDGE_ARGUMENT"
        );
    }

    #[test]
    fn rejects_mux_arm_width_mismatch() {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(0),
                    RegisterId(1),
                    RegisterId(2),
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(7)),
            ],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.MUX_ELSE_WIDTH"
        );
    }

    #[test]
    fn rejects_non_boolean_operator_results() {
        let comparison = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(0),
                    RegisterId(1),
                    BinaryOp::LtU,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        assert_eq!(
            comparison.verify_result().unwrap_err().invariant,
            "TYPE.COMPARISON_RESULT_WIDTH"
        );

        let reduction = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(0),
                    UnaryOp::Or,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
        );
        assert_eq!(
            reduction.verify_result().unwrap_err().invariant,
            "TYPE.REDUCTION_RESULT_WIDTH"
        );
    }

    #[test]
    fn rejects_width_changing_two_state_conversion() {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(0),
                    UnaryOp::ToTwoState,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(4))],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.TWO_STATE_WIDTH"
        );
    }

    #[test]
    fn rejects_non_bit_two_state_destination() {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(0),
                    UnaryOp::ToTwoState,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), logic(8)), (RegisterId(1), logic(8))],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.TWO_STATE_DESTINATION"
        );
    }

    #[test]
    fn rejects_width_changing_value_unary() {
        for op in [UnaryOp::Minus, UnaryOp::BitNot] {
            let eu = unit(
                [BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![SIRInstruction::Unary(RegisterId(0), op, RegisterId(1))],
                    terminator: SIRTerminator::Return,
                }],
                [(RegisterId(0), bit(8)), (RegisterId(1), bit(4))],
            );
            assert_eq!(
                eu.verify_result().unwrap_err().invariant,
                "TYPE.UNARY_VALUE_WIDTH"
            );
        }
    }

    #[test]
    fn rejects_signed_bit_boolean_result() {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(0),
                    RegisterId(1),
                    BinaryOp::Eq,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), signed_bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.BOOLEAN_RESULT_UNSIGNED"
        );
    }

    #[test]
    fn rejects_comparison_operand_width_mismatch() {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(0),
                    RegisterId(1),
                    BinaryOp::LtS,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(4)),
            ],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.COMPARISON_OPERAND_WIDTH"
        );
    }

    #[test]
    fn rejects_divrem_width_mismatch() {
        for op in [
            BinaryOp::DivU,
            BinaryOp::DivS,
            BinaryOp::RemU,
            BinaryOp::RemS,
        ] {
            let eu = unit(
                [BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![SIRInstruction::Binary(
                        RegisterId(0),
                        RegisterId(1),
                        op,
                        RegisterId(2),
                    )],
                    terminator: SIRTerminator::Return,
                }],
                [
                    (RegisterId(0), bit(8)),
                    (RegisterId(1), bit(8)),
                    (RegisterId(2), bit(4)),
                ],
            );
            assert_eq!(
                eu.verify_result().unwrap_err().invariant,
                "TYPE.DIVREM_RHS_WIDTH"
            );
        }
    }

    #[test]
    fn rejects_masked_bit_immediate() {
        let eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![SIRInstruction::Imm(
                    RegisterId(0),
                    SIRValue::new_four_state(1u8, 1u8),
                )],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(1))],
        );
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.BIT_IMMEDIATE_MASK"
        );
    }

    #[test]
    fn rejects_noncanonical_branch_condition() {
        for condition_type in [logic(1), bit(2), signed_bit(1)] {
            let eu = unit(
                [
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![],
                        instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8))],
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(0),
                            true_block: (BlockId(1), vec![]),
                            false_block: (BlockId(2), vec![]),
                        },
                    },
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Return,
                    },
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        instructions: vec![],
                        terminator: SIRTerminator::Return,
                    },
                ],
                [(RegisterId(0), condition_type)],
            );
            assert_eq!(
                eu.verify_result().unwrap_err().invariant,
                "TYPE.BRANCH_CONDITION"
            );
        }
    }

    fn bit_count_unit(
        op: UnaryOp,
        source_width: usize,
        result_width: usize,
    ) -> ExecutionUnit<usize> {
        unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                    SIRInstruction::Unary(RegisterId(1), op, RegisterId(0)),
                ],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(source_width)),
                (RegisterId(1), bit(result_width)),
            ],
        )
    }

    #[test]
    fn accepts_bit_count_result_that_can_represent_source_width() {
        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            assert_eq!(bit_count_unit(op, 8, 4).verify_result(), Ok(()));
            assert_eq!(bit_count_unit(op, 8, 8).verify_result(), Ok(()));
        }
    }

    #[test]
    fn rejects_bit_count_result_that_cannot_represent_source_width() {
        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            let error = bit_count_unit(op, 8, 3).verify_result().unwrap_err();
            assert_eq!(error.invariant, "TYPE.BIT_COUNT_RESULT_WIDTH");
            assert_eq!(error.block, Some(BlockId(0)));
            assert_eq!(error.instruction, Some(1));
            assert!(error.message.contains("requires at least 4 result bits"));
        }
    }
}
