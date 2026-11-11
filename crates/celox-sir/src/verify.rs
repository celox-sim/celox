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

    pub fn instruction(
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
        SIRTerminator::Switch {
            selector,
            cases,
            default,
        } => {
            let selector_ty = register_type(eu, block.id, None, *selector)?;
            let width = selector_ty.width();
            if width == 0 || width > 8 {
                return Err(SirVerifyError::block(
                    "TYPE.SWITCH_SELECTOR",
                    block.id,
                    format!(
                        "switch selector r{} has width {}, expected 1..=8",
                        selector.0, width
                    ),
                ));
            }
            let limit = BigUint::from(1u8) << width;
            let mut values = crate::HashSet::default();
            for case in cases {
                if case.value >= limit {
                    return Err(SirVerifyError::block(
                        "TYPE.SWITCH_CASE",
                        block.id,
                        format!(
                            "switch case {:#x} does not fit selector width {}",
                            case.value, width
                        ),
                    ));
                }
                if !values.insert(case.value.clone()) {
                    return Err(SirVerifyError::block(
                        "CFG.SWITCH_DUPLICATE",
                        block.id,
                        format!("duplicate switch case {:#x}", case.value),
                    ));
                }
            }
            cases
                .iter()
                .map(|case| (case.target, &[][..]))
                .chain(std::iter::once((*default, &[][..])))
                .collect()
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
            BinaryOp::Float { op, precision } => {
                exact_width(*lhs, precision.width(), "TYPE.FLOAT_OPERAND_WIDTH")?;
                exact_width(*rhs, precision.width(), "TYPE.FLOAT_OPERAND_WIDTH")?;
                let comparison = matches!(
                    op,
                    crate::FloatBinaryOp::Eq
                        | crate::FloatBinaryOp::Ne
                        | crate::FloatBinaryOp::Lt
                        | crate::FloatBinaryOp::Le
                        | crate::FloatBinaryOp::Gt
                        | crate::FloatBinaryOp::Ge
                );
                exact_width(
                    *dst,
                    if comparison { 1 } else { precision.width() },
                    "TYPE.FLOAT_RESULT_WIDTH",
                )?;
            }
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::EqCase
            | BinaryOp::NeCase
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
            UnaryOp::Float {
                op: float_op,
                precision,
            } => {
                let input_width = match float_op {
                    crate::FloatUnaryOp::FromSigned { width }
                    | crate::FloatUnaryOp::FromUnsigned { width } => *width,
                    _ => precision.width(),
                };
                exact_width(*src, input_width, "TYPE.FLOAT_OPERAND_WIDTH")?;
                exact_width(
                    *dst,
                    op.result_width(input_width),
                    "TYPE.FLOAT_RESULT_WIDTH",
                )?;
                if input_width == 0 || input_width > 64 || ty(*dst)?.width() > 64 {
                    return Err(SirVerifyError::instruction(
                        "TYPE.FLOAT_CONVERSION_WIDTH",
                        block,
                        index,
                        "floating conversions require integer widths in 1..=64",
                    ));
                }
            }
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
            // Planes move whole 64-bit words.
            UnaryOp::ToPlanes | UnaryOp::FromPlanes
                if {
                    let words = if *op == UnaryOp::ToPlanes { 64 } else { 128 };
                    let width = ty(*src)?.width();
                    width == 0 || width % words != 0
                } =>
            {
                return Err(SirVerifyError::instruction(
                    "TYPE.PLANES_WORDS",
                    block,
                    index,
                    format!("r{} is not a whole number of 64-bit planes words", src.0),
                ));
            }
            UnaryOp::ToPlanes => {
                exact_width(*dst, 2 * ty(*src)?.width(), "TYPE.PLANES_WIDTH")?;
                if !matches!(ty(*dst)?, RegisterType::Bit { signed: false, .. }) {
                    return Err(SirVerifyError::instruction(
                        "TYPE.PLANES_DESTINATION",
                        block,
                        index,
                        format!("r{} is not an unsigned Bit register", dst.0),
                    ));
                }
            }
            UnaryOp::FromPlanes => {
                exact_width(*src, 2 * ty(*dst)?.width(), "TYPE.PLANES_WIDTH")?;
                if !matches!(ty(*src)?, RegisterType::Bit { .. }) {
                    return Err(SirVerifyError::instruction(
                        "TYPE.PLANES_SOURCE",
                        block,
                        index,
                        format!("r{} is not a Bit register", src.0),
                    ));
                }
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
        SIRInstruction::ExternCall { dst, func, args } => {
            if let Some(op) = crate::native_memory::NativeMemoryOp::decode(*func) {
                let words = args.iter().chain(dst).all(|reg| {
                    matches!(
                        eu.register_map.get(reg),
                        Some(RegisterType::Bit {
                            width: 64,
                            signed: false
                        })
                    )
                });
                if args.len() != op.arguments() || dst.is_some() != op.reads() || !words {
                    return Err(SirVerifyError::instruction(
                        "TYPE.NATIVE_MEMORY_SIGNATURE",
                        block,
                        index,
                        "native memory operations require their exact unsigned 64-bit signature",
                    ));
                }
            }
            if crate::extern_abi::native_kernel_index(*func).is_some()
                && (dst.is_some() || !args.is_empty())
            {
                return Err(SirVerifyError::instruction(
                    "TYPE.NATIVE_KERNEL_CALL_SIGNATURE",
                    block,
                    index,
                    "a native kernel call has only an implicit state pointer",
                ));
            }
            if *func == crate::extern_abi::HEAP_HELPER_ID
                && args.len() != crate::extern_abi::HEAP_HELPER_ARGUMENTS
            {
                return Err(SirVerifyError::instruction(
                    "TYPE.HEAP_CALL_ARGUMENT_COUNT",
                    block,
                    index,
                    format!(
                        "a heap call takes {} arguments, not {}",
                        crate::extern_abi::HEAP_HELPER_ARGUMENTS,
                        args.len()
                    ),
                ));
            }
            if args.len() > crate::MAX_EXTERN_CALL_ARGUMENTS {
                return Err(SirVerifyError::instruction(
                    "TYPE.EXTERN_CALL_ARGUMENT_COUNT",
                    block,
                    index,
                    format!(
                        "{} arguments exceed the limit of {}",
                        args.len(),
                        crate::MAX_EXTERN_CALL_ARGUMENTS
                    ),
                ));
            }
            for &reg in args.iter().chain(dst) {
                if crate::extern_abi::ExternValue::of(ty(reg)?).is_none() {
                    return Err(SirVerifyError::instruction(
                        "TYPE.EXTERN_CALL_C_INTEGER",
                        block,
                        index,
                        format!(
                            "r{} is neither a Bit of 1, 8, 16, 32 or 64 bits nor a one-bit Logic",
                            reg.0
                        ),
                    ));
                }
            }
        }
        // The callee's frame is checked against the program, see
        // `SirProgram::verify_functions`.
        SIRInstruction::Call {
            frame, args, dst, ..
        } => {
            if !matches!(ty(*frame)?, RegisterType::Bit { width, .. } if *width <= 64) {
                return Err(SirVerifyError::instruction(
                    "CALL.FRAME_BIT",
                    block,
                    index,
                    format!("frame r{} is not a Bit of at most 64 bits", frame.0),
                ));
            }
            for &(_, reg) in args {
                ty(reg)?;
            }
            if let Some(dst) = dst {
                ty(*dst)?;
            }
        }
    }
    Ok(())
}

impl<EventAddr, StateAddr: Ord + Clone + std::hash::Hash> crate::SirProgram<EventAddr, StateAddr> {
    /// Verify the program's function units and their calls:
    ///
    /// - each frame slot lies within its frame, and no two slots of the
    ///   program share an address;
    /// - a frame slot is accessed only by its own function's unit, within the
    ///   slot, and never committed;
    /// - a frame slot is accessed at a static offset, or one holding an
    ///   unpacked array also at a dynamic one within one of its elements;
    /// - every [`SIRInstruction::Call`] names a function of the program,
    ///   stores each argument into a slot of the callee of the argument's
    ///   width, and has a `dst` exactly as wide as the outputs it reads;
    /// - the effects of each function and of each call of it are the current
    ///   ones (see [`crate::SirProgram::summarize_calls`]).
    pub fn verify_functions(&self) -> Result<(), SirVerifyError> {
        let mut owners: crate::HashMap<&StateAddr, (usize, &crate::FrameSlot<StateAddr>)> =
            crate::HashMap::default();
        for (function, unit) in self.functions.iter().enumerate() {
            for slot in &unit.frame.slots {
                if slot.offset + slot.size() > unit.frame.size {
                    return Err(SirVerifyError::function(
                        "FRAME.SLOT_IN_FRAME",
                        format!(
                            "a slot of fn{function} ends at byte {}, past its frame of {}",
                            slot.offset + slot.size(),
                            unit.frame.size
                        ),
                    ));
                }
                if owners.insert(&slot.addr, (function, slot)).is_some() {
                    return Err(SirVerifyError::function(
                        "FRAME.SLOT_ADDRESS_UNIQUE",
                        format!("a slot address of fn{function} names another slot too"),
                    ));
                }
            }
        }
        let effects = self.call_effects();
        for (function, (unit, current)) in self.functions.iter().zip(&effects).enumerate() {
            if *unit.effects != *current {
                return Err(SirVerifyError::function(
                    "CALL.EFFECTS_CURRENT",
                    format!("the effects of fn{function} are not its current ones"),
                ));
            }
        }
        let check = |unit: &ExecutionUnit<StateAddr>,
                     own: Option<usize>|
         -> Result<(), SirVerifyError> {
            for block in unit.blocks.values() {
                for (index, inst) in block.instructions.iter().enumerate() {
                    let accesses = [inst.memory_read(), inst.memory_write()];
                    for access in accesses.into_iter().flatten() {
                        let Some(&(owner, slot)) = owners.get(access.addr) else {
                            continue;
                        };
                        if own != Some(owner) || matches!(inst, SIRInstruction::Commit(..)) {
                            return Err(SirVerifyError::instruction(
                                "FRAME.SLOT_OWN_FUNCTION",
                                block.id,
                                index,
                                format!("accesses a frame slot of fn{owner} outside fn{owner}"),
                            ));
                        }
                        // A slot holding an unpacked array is also accessed
                        // at a position computed at run time, within one of
                        // its elements; the producer keeps the position in
                        // the array, as for an array of the state.
                        let offset = match (access.offset, slot.element_width) {
                            (crate::SIROffset::Static(offset), _)
                            | (
                                crate::SIROffset::PackedElements {
                                    bit_offset: offset, ..
                                },
                                Some(_),
                            ) => *offset,
                            (
                                crate::SIROffset::Element {
                                    element_width,
                                    bit_offset,
                                    dynamic_bit_offset,
                                    ..
                                },
                                Some(slot_element),
                            ) if *element_width == slot_element
                                && access.width <= slot_element
                                && (dynamic_bit_offset.is_some()
                                    || bit_offset + access.width <= slot_element) =>
                            {
                                continue;
                            }
                            (crate::SIROffset::Dynamic(_), Some(slot_element))
                                if access.width <= slot_element =>
                            {
                                continue;
                            }
                            _ => {
                                return Err(SirVerifyError::instruction(
                                    "FRAME.STATIC_OFFSET",
                                    block.id,
                                    index,
                                    format!("accesses a frame slot at offset {}", access.offset),
                                ));
                            }
                        };
                        if offset + access.width > slot.width {
                            return Err(SirVerifyError::instruction(
                                "FRAME.ACCESS_IN_SLOT",
                                block.id,
                                index,
                                format!(
                                    "accesses bits [{offset}..{}) of a {}-bit frame slot",
                                    offset + access.width,
                                    slot.width
                                ),
                            ));
                        }
                    }
                    let SIRInstruction::Call {
                        function,
                        args,
                        outputs,
                        dst,
                        effects: call_effects,
                        ..
                    } = inst
                    else {
                        continue;
                    };
                    let error = |invariant, message: String| {
                        SirVerifyError::instruction(invariant, block.id, index, message)
                    };
                    let Some(callee) = self.functions.get(function.0 as usize) else {
                        return Err(error(
                            "CALL.FUNCTION_EXISTS",
                            format!("{function} is not a function of the program"),
                        ));
                    };
                    if **call_effects != effects[function.0 as usize] {
                        return Err(error(
                            "CALL.EFFECTS_CURRENT",
                            format!("the call's effects are not those of {function}"),
                        ));
                    }
                    let slot = |position: usize| {
                        callee.frame.slots.get(position).ok_or_else(|| {
                            error(
                                "CALL.SLOT_EXISTS",
                                format!("{function} has no frame slot {position}"),
                            )
                        })
                    };
                    for &(position, arg) in args {
                        let slot = slot(position)?;
                        let width = unit.register_map.get(&arg).map(RegisterType::width);
                        if width != Some(slot.width) {
                            return Err(error(
                                "CALL.ARGUMENT_WIDTH",
                                format!(
                                    "r{} of {width:?} bits is stored into a {}-bit slot",
                                    arg.0, slot.width
                                ),
                            ));
                        }
                    }
                    let mut width = 0;
                    for &position in outputs {
                        width += slot(position)?.width;
                    }
                    let dst_width = dst
                        .and_then(|dst| unit.register_map.get(&dst))
                        .map(RegisterType::width);
                    let expected = (!outputs.is_empty()).then_some(width);
                    if dst_width != expected {
                        return Err(error(
                            "CALL.OUTPUT_WIDTH",
                            format!(
                                "the outputs are {expected:?} bits, the destination {dst_width:?}"
                            ),
                        ));
                    }
                }
            }
            Ok(())
        };
        self.units().try_for_each(|unit| check(unit, None))?;
        self.functions
            .iter()
            .enumerate()
            .try_for_each(|(function, unit)| check(&unit.unit, Some(function)))
    }
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
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => {
            if *element_width == 0 {
                return Err(SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_WIDTH_NON_ZERO",
                    block,
                    index,
                    "packed-elements element width is zero",
                ));
            }
            let Some(access_end) = bit_offset.checked_add(access_width) else {
                return Err(SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_ACCESS_IN_BOUNDS",
                    block,
                    index,
                    "packed-elements access range overflows usize",
                ));
            };
            if *bit_offset % *element_width != 0 || access_end % *element_width != 0 {
                return Err(SirVerifyError::instruction(
                    "MEMORY.PACKED_ELEMENTS_ACCESS_ALIGNED",
                    block,
                    index,
                    format!(
                        "packed-elements access [{bit_offset}..{access_end}) is not aligned to element width {element_width}"
                    ),
                ));
            }
        }
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
    inst.defined_register()
}

fn instruction_uses<A>(inst: &SIRInstruction<A>) -> Vec<RegisterId> {
    // A zero-width store moves no bits, so its operands need no definition.
    if matches!(inst, SIRInstruction::Store(_, _, 0, _, _, _)) {
        return Vec::new();
    }
    let mut uses = Vec::new();
    inst.for_each_use(|register| uses.push(register));
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
        SIRTerminator::Switch { selector, .. } => vec![*selector],
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
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
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
mod tests;
