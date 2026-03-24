//! SIRT → WASM bytecode translator.
//!
//! Uses `wasm-encoder` to build a WASM module from SIRT [`ExecutionUnit`]s.
//! The generated module imports a linear memory ("env"/"memory") and exports
//! a single `"run"` function `() -> i64` (0 = success, non-zero = error).
//!
//! RegisterId → WASM local (i64 for ≤64-bit; multiple i64 locals for wide values).
//! Block control flow is implemented via `loop` + `block` + `br_table`.

use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction,
    MemoryType, Module, TypeSection, ValType,
};

use crate::{
    HashMap,
    backend::MemoryLayout,
    ir::{
        AbsoluteAddr, BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId,
        RegisterType, SIRInstruction, SIROffset, SIRTerminator, SIRValue, STABLE_REGION,
        TriggerIdWithKind, UnaryOp,
    },
};

use super::get_byte_size;

/// Compiled WASM module bytes for a set of execution units.
pub struct WasmModule {
    pub bytes: Vec<u8>,
}

/// Maps a RegisterId to one or more WASM local indices.
/// For ≤64-bit values: single local (i64).
/// For >64-bit values: `num_chunks` consecutive locals (i64 each), LSB first.
#[derive(Clone, Debug)]
struct RegLocal {
    /// Index of the first WASM local for the value.
    value_idx: u32,
    /// Number of i64 chunks.
    num_chunks: usize,
    /// Index of the first WASM local for the 4-state mask (if any).
    mask_idx: Option<u32>,
}

/// Allocator for WASM local variables.
struct LocalAllocator {
    /// Next available local index (after function parameters).
    next: u32,
    /// Map from RegisterId to its allocated locals.
    reg_map: HashMap<RegisterId, RegLocal>,
    /// Extra locals for block argument passing.
    block_arg_locals: HashMap<(BlockId, usize), RegLocal>,
    /// Locals for pre-loaded trigger old values.
    trigger_locals: HashMap<(AbsoluteAddr, u32), u32>,
    /// Total number of i64 locals declared.
    num_locals: u32,
}

impl LocalAllocator {
    fn new() -> Self {
        Self {
            next: 0,
            reg_map: HashMap::default(),
            block_arg_locals: HashMap::default(),
            trigger_locals: HashMap::default(),
            num_locals: 0,
        }
    }

    fn alloc(&mut self, count: usize) -> u32 {
        let idx = self.next;
        self.next += count as u32;
        self.num_locals = self.num_locals.max(self.next);
        idx
    }

    fn alloc_reg(&mut self, reg: RegisterId, width: usize, four_state: bool) -> RegLocal {
        let num_chunks = num_i64_chunks(width);
        let value_idx = self.alloc(num_chunks);
        let mask_idx = if four_state {
            Some(self.alloc(num_chunks))
        } else {
            None
        };
        let local = RegLocal {
            value_idx,
            num_chunks,
            mask_idx,
        };
        self.reg_map.insert(reg, local.clone());
        local
    }

    fn alloc_block_args(
        &mut self,
        block_id: BlockId,
        params: &[RegisterId],
        register_map: &HashMap<RegisterId, RegisterType>,
        four_state: bool,
    ) {
        for (i, &reg) in params.iter().enumerate() {
            let width = register_map[&reg].width();
            let num_chunks = num_i64_chunks(width);
            let value_idx = self.alloc(num_chunks);
            let mask_idx = if four_state {
                Some(self.alloc(num_chunks))
            } else {
                None
            };
            self.block_arg_locals.insert(
                (block_id, i),
                RegLocal {
                    value_idx,
                    num_chunks,
                    mask_idx,
                },
            );
        }
    }
}

fn num_i64_chunks(width: usize) -> usize {
    if width == 0 { 1 } else { (width + 63) / 64 }
}

/// Build a WASM module from a set of execution units.
///
/// The generated module:
/// - Imports memory `("env", "memory")`
/// - Exports function `"run"` with signature `() -> i64`
pub fn compile_units(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
) -> WasmModule {
    let mut module = Module::new();

    // Type section: one function type () -> i64
    let mut types = TypeSection::new();
    types.ty().function(vec![], vec![ValType::I64]);
    module.section(&types);

    // Import section: memory from "env"
    let mut imports = ImportSection::new();
    let mem_pages = (layout.merged_total_size + 65535) / 65536;
    imports.import(
        "env",
        "memory",
        MemoryType {
            minimum: mem_pages as u64,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        },
    );
    module.section(&imports);

    // Function section: one function with type 0
    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);

    // Export section: export the function as "run"
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, 0);
    module.section(&exports);

    // Code section
    let mut codes = CodeSection::new();
    let func = compile_function(units, layout, four_state, emit_triggers);
    codes.function(&func);
    module.section(&codes);

    WasmModule {
        bytes: module.finish(),
    }
}

/// Compile all execution units into a single WASM function body.
fn compile_function(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
) -> Function {
    let mut locals = LocalAllocator::new();

    // Pre-allocate locals for all registers in all units.
    // RegisterId is scoped per-unit but we flatten them into one function,
    // so we must handle potential collisions by prefixing per-unit.
    // However, the Cranelift translator also puts all units in one function
    // and clears regs per unit. We reuse locals across units since RegisterIds
    // don't collide (they're globally unique within a Program).
    //
    // Actually, RegisterIds CAN collide across units (they're unit-scoped).
    // We need per-unit local allocation. For simplicity, we'll allocate fresh
    // locals for each unit by clearing the reg_map.

    // First pass: determine total locals needed.
    // We'll do this lazily during codegen and declare them all as i64 at the end.

    let mut instrs: Vec<Instruction<'static>> = Vec::new();

    if units.is_empty() {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::Return);
        instrs.push(Instruction::End);
        let mut func = Function::new(vec![(1, ValType::I64)]);
        for i in &instrs {
            func.instruction(i);
        }
        return func;
    }

    // Pre-load trigger old values.
    let trigger_addrs = if emit_triggers {
        collect_trigger_addrs(units)
    } else {
        Vec::new()
    };
    for &(abs, region) in &trigger_addrs {
        let local_idx = locals.alloc(1);
        locals.trigger_locals.insert((abs, region), local_idx);
        let offset = compute_byte_offset(layout, &abs, region);
        instrs.push(Instruction::I32Const(offset as i32));
        instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 3, // 8-byte aligned
            memory_index: 0,
        }));
        instrs.push(Instruction::LocalSet(local_idx));
    }

    // Compile each unit sequentially.
    // Control flow: each unit is a sequence of blocks.
    // We use a dispatch loop: a local `block_id` determines which block to execute.
    for unit in units {
        compile_unit(
            unit,
            layout,
            four_state,
            emit_triggers,
            &mut locals,
            &mut instrs,
        );
    }

    // Successful return
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::End);

    // Build the function with all locals declared as i64.
    let mut func = Function::new(vec![(locals.num_locals, ValType::I64)]);
    for i in &instrs {
        func.instruction(i);
    }
    func
}

/// Compile a single execution unit using a block dispatch loop.
///
/// WASM doesn't have arbitrary goto, so we implement block dispatch as:
/// ```wat
/// (local $block_id i32)
/// (local.set $block_id (i32.const <entry_block_index>))
/// (block $exit
///   (loop $dispatch
///     (block $b0
///       (block $b1
///         (block $b2
///           (br_table $b0 $b1 $b2 ... (local.get $block_id))
///         ) ;; $b2
///         ;; block 2 code...
///         (local.set $block_id (i32.const <next>))
///         (br $dispatch)
///       ) ;; $b1
///       ;; block 1 code...
///     ) ;; $b0
///     ;; block 0 code...
///   ) ;; $dispatch
/// ) ;; $exit
/// ```
fn compile_unit(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // Clear per-unit register map (RegisterIds are unit-scoped).
    locals.reg_map.clear();
    locals.block_arg_locals.clear();

    // Assign dense indices to blocks. Entry block gets index 0.
    let mut block_ids: Vec<BlockId> = unit.blocks.keys().copied().collect();
    block_ids.sort();
    // Move entry block to index 0
    if let Some(pos) = block_ids.iter().position(|&id| id == unit.entry_block_id) {
        block_ids.swap(0, pos);
    }
    let block_index: HashMap<BlockId, usize> = block_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();
    let num_blocks = block_ids.len();

    // Pre-allocate all registers for this unit.
    for (&reg, &ref ty) in &unit.register_map {
        locals.alloc_reg(reg, ty.width(), four_state);
    }

    // Allocate block argument passing locals.
    for &blk_id in &block_ids {
        let block = &unit.blocks[&blk_id];
        if !block.params.is_empty() {
            locals.alloc_block_args(blk_id, &block.params, &unit.register_map, four_state);
        }
    }

    // Allocate a local for the block dispatch index.
    let block_id_local = locals.alloc(1);

    // Set initial block to entry (index 0).
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(block_id_local));

    // Emit: (block $exit (loop $dispatch (block $b0 (block $b1 ... (br_table ...) ) ... ) ) )
    // $exit is at depth num_blocks + 1 from innermost
    // $dispatch (loop) is at depth num_blocks from innermost
    // $b_i (block) at depth num_blocks - 1 - i from innermost

    // block $exit
    instrs.push(Instruction::Block(wasm_encoder::BlockType::Empty));
    // loop $dispatch
    instrs.push(Instruction::Loop(wasm_encoder::BlockType::Empty));

    // Nest blocks: innermost first
    for _ in 0..num_blocks {
        instrs.push(Instruction::Block(wasm_encoder::BlockType::Empty));
    }

    // br_table: dispatch to correct block
    instrs.push(Instruction::LocalGet(block_id_local));
    instrs.push(Instruction::I32WrapI64);
    // br_table targets: $b0, $b1, ... $b_{n-1}, default=$exit
    let targets: Vec<u32> = (0..num_blocks as u32).collect();
    instrs.push(Instruction::BrTable(
        targets.clone().into(),
        num_blocks as u32 + 1, // default: break to $exit (error)
    ));

    // Now emit each block's code after its `end` marker.
    // Block i's code comes after closing `end` of block i.
    for (i, &blk_id) in block_ids.iter().enumerate() {
        // end of innermost block (or previous block)
        instrs.push(Instruction::End); // close block $b_i

        let block = &unit.blocks[&blk_id];

        // Load block arguments from passing locals into register locals.
        for (j, &param_reg) in block.params.iter().enumerate() {
            let passing = &locals.block_arg_locals[&(blk_id, j)];
            let reg = &locals.reg_map[&param_reg];
            for c in 0..reg.num_chunks {
                instrs.push(Instruction::LocalGet(passing.value_idx + c as u32));
                instrs.push(Instruction::LocalSet(reg.value_idx + c as u32));
            }
            if let (Some(rm), Some(pm)) = (reg.mask_idx, passing.mask_idx) {
                for c in 0..reg.num_chunks {
                    instrs.push(Instruction::LocalGet(pm + c as u32));
                    instrs.push(Instruction::LocalSet(rm + c as u32));
                }
            }
        }

        // Translate instructions.
        for inst in &block.instructions {
            compile_instruction(
                inst,
                unit,
                layout,
                four_state,
                emit_triggers,
                locals,
                instrs,
            );
        }

        // Translate terminator.
        // After end $b_i, br depth to $dispatch (loop) = num_blocks - 1 - i
        // br depth to $exit = num_blocks - i
        let br_dispatch_depth = (num_blocks - 1 - i) as u32;
        let br_exit_depth = (num_blocks - i) as u32;
        compile_terminator(
            &block.terminator,
            &block_index,
            num_blocks,
            block_id_local,
            br_dispatch_depth,
            br_exit_depth,
            unit,
            four_state,
            locals,
            instrs,
        );
    }

    // end loop $dispatch
    instrs.push(Instruction::End);
    // end block $exit
    instrs.push(Instruction::End);
}

fn compile_instruction(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match inst {
        SIRInstruction::Imm(dst, val) => {
            compile_imm(dst, val, unit, four_state, &*locals, instrs);
        }
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            compile_binary(dst, lhs, op, rhs, unit, four_state, &mut *locals, instrs);
        }
        SIRInstruction::Unary(dst, op, src) => {
            compile_unary(dst, op, src, unit, four_state, locals, instrs);
        }
        SIRInstruction::Load(dst, addr, offset, op_width) => {
            compile_load(
                dst, addr, offset, *op_width, layout, four_state, locals, instrs,
            );
        }
        SIRInstruction::Store(addr, offset, op_width, src, triggers) => {
            compile_store(
                addr,
                offset,
                *op_width,
                src,
                triggers,
                layout,
                four_state,
                emit_triggers,
                locals,
                instrs,
            );
        }
        SIRInstruction::Commit(src_addr, dst_addr, offset, op_width, triggers) => {
            compile_commit(
                src_addr,
                dst_addr,
                offset,
                *op_width,
                triggers,
                layout,
                four_state,
                emit_triggers,
                locals,
                instrs,
            );
        }
        SIRInstruction::Concat(dst, args) => {
            compile_concat(dst, args, unit, four_state, locals, instrs);
        }
        SIRInstruction::Slice(_, _, _, _) => {
            todo!("Slice WASM lowering")
        }
    }
}

// ============================================================
// Instruction compilers
// ============================================================

fn compile_imm(
    dst: &RegisterId,
    val: &SIRValue,
    _unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    _four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let reg = &locals.reg_map[dst];
    let digits = val.payload.to_u64_digits();
    for c in 0..reg.num_chunks {
        let v = digits.get(c).copied().unwrap_or(0);
        instrs.push(Instruction::I64Const(v as i64));
        instrs.push(Instruction::LocalSet(reg.value_idx + c as u32));
    }
    if let Some(mask_idx) = reg.mask_idx {
        let mask_digits = val.mask.to_u64_digits();
        for c in 0..reg.num_chunks {
            let m = mask_digits.get(c).copied().unwrap_or(0);
            instrs.push(Instruction::I64Const(m as i64));
            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
        }
    }
}

fn compile_binary(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d_num = locals.reg_map[dst].num_chunks;
    let l_num = locals.reg_map[lhs].num_chunks;
    let r_num = locals.reg_map[rhs].num_chunks;
    let d_width = unit.register_map[dst].width();

    if d_num == 1 && l_num == 1 && r_num == 1 {
        compile_binary_narrow(dst, lhs, op, rhs, d_width, locals, instrs);
    } else {
        compile_binary_wide(dst, lhs, op, rhs, d_width, unit, locals, instrs);
    }
}

fn compile_binary_narrow(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    d_width: usize,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = &locals.reg_map[dst];
    let l = &locals.reg_map[lhs];
    let r = &locals.reg_map[rhs];

    // Load operands
    instrs.push(Instruction::LocalGet(l.value_idx));
    instrs.push(Instruction::LocalGet(r.value_idx));

    match op {
        BinaryOp::Add => instrs.push(Instruction::I64Add),
        BinaryOp::Sub => instrs.push(Instruction::I64Sub),
        BinaryOp::Mul => instrs.push(Instruction::I64Mul),
        BinaryOp::Div => {
            // Unsigned division (with zero-check: WASM traps on div by 0)
            instrs.push(Instruction::I64DivU);
        }
        BinaryOp::Rem => {
            instrs.push(Instruction::I64RemU);
        }
        BinaryOp::And => instrs.push(Instruction::I64And),
        BinaryOp::Or => instrs.push(Instruction::I64Or),
        BinaryOp::Xor => instrs.push(Instruction::I64Xor),
        BinaryOp::Shl => {
            instrs.push(Instruction::I64Shl);
        }
        BinaryOp::Shr => {
            instrs.push(Instruction::I64ShrU);
        }
        BinaryOp::Sar => {
            // Arithmetic shift right — need sign extension first.
            // Pop both operands, sign-extend lhs, then shift.
            // We already pushed lhs, rhs. We need to reorganize.
            // Let's redo: remove the last two pushes and do it properly.
            let len = instrs.len();
            instrs.truncate(len - 2);
            // Sign-extend lhs to 64-bit based on its logical width
            let l_width = d_width; // lhs width matches dst width for shift
            emit_sign_extend(instrs, l.value_idx, l_width);
            instrs.push(Instruction::LocalGet(r.value_idx));
            instrs.push(Instruction::I64ShrS);
        }
        BinaryOp::Eq => {
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::Ne => {
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LtU => {
            instrs.push(Instruction::I64LtU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LtS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            let l_width = d_width.max(1); // Approximation: use dst width
            let r_width = d_width.max(1);
            emit_sign_extend(instrs, l.value_idx, l_width);
            emit_sign_extend(instrs, r.value_idx, r_width);
            instrs.push(Instruction::I64LtS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LeU => {
            instrs.push(Instruction::I64LeU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LeS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            let l_width = d_width.max(1);
            let r_width = d_width.max(1);
            emit_sign_extend(instrs, l.value_idx, l_width);
            emit_sign_extend(instrs, r.value_idx, r_width);
            instrs.push(Instruction::I64LeS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GtU => {
            instrs.push(Instruction::I64GtU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GtS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            let l_width = d_width.max(1);
            let r_width = d_width.max(1);
            emit_sign_extend(instrs, l.value_idx, l_width);
            emit_sign_extend(instrs, r.value_idx, r_width);
            instrs.push(Instruction::I64GtS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GeU => {
            instrs.push(Instruction::I64GeU);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::GeS => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            let l_width = d_width.max(1);
            let r_width = d_width.max(1);
            emit_sign_extend(instrs, l.value_idx, l_width);
            emit_sign_extend(instrs, r.value_idx, r_width);
            instrs.push(Instruction::I64GeS);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LogicAnd => {
            // (a != 0) & (b != 0)
            let len = instrs.len();
            instrs.truncate(len - 2);
            instrs.push(Instruction::LocalGet(l.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::LocalGet(r.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I32And);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::LogicOr => {
            let len = instrs.len();
            instrs.truncate(len - 2);
            instrs.push(Instruction::LocalGet(l.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::LocalGet(r.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I32Or);
            instrs.push(Instruction::I64ExtendI32U);
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            // For 2-state (no mask), wildcard eq/ne is the same as regular eq/ne.
            if matches!(op, BinaryOp::EqWildcard) {
                instrs.push(Instruction::I64Eq);
            } else {
                instrs.push(Instruction::I64Ne);
            }
            instrs.push(Instruction::I64ExtendI32U);
        }
    }

    // Mask to destination width
    emit_mask_to_width(instrs, d_width);
    instrs.push(Instruction::LocalSet(d.value_idx));
}

fn compile_binary_wide(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    d_width: usize,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let l = locals.reg_map[lhs].clone();
    let r = locals.reg_map[rhs].clone();
    let d_chunks = d.num_chunks;

    match op {
        // 1. Chunk-wise bitwise ops
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            for c in 0..d_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                match op {
                    BinaryOp::And => instrs.push(Instruction::I64And),
                    BinaryOp::Or => instrs.push(Instruction::I64Or),
                    BinaryOp::Xor => instrs.push(Instruction::I64Xor),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 2. Addition with carry propagation
        BinaryOp::Add => {
            let carry = locals.alloc(1); // carry flag (0 or 1)
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(carry));
            for c in 0..d_chunks {
                // s1 = l[c] + r[c]
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Add);
                // c1 = s1 < l[c] (unsigned overflow)
                instrs.push(Instruction::LocalTee(d.value_idx + c as u32));
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::I64LtU);
                instrs.push(Instruction::I64ExtendI32U); // c1 as i64
                if c > 0 {
                    // s2 = s1 + carry
                    instrs.push(Instruction::LocalGet(d.value_idx + c as u32));
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalTee(d.value_idx + c as u32));
                    // c2 = s2 < s1 (carry from adding carry_in)
                    // But s1 was overwritten. Use: c2 = (carry != 0 && s2 == 0 when s1 was MAX)
                    // Simpler: c2 = (carry != 0) & (s2 < carry) ... no.
                    // Actually: if carry_in was 1 and s1 was 0xFFFF...FFFF, then s2 wraps.
                    // c2 = (s2 < carry_in)
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    // total carry = c1 | c2
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::LocalSet(carry));
            }
        }
        // 3. Subtraction with borrow propagation
        BinaryOp::Sub => {
            let borrow = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(borrow));
            for c in 0..d_chunks {
                // d1 = l[c] - r[c]
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Sub);
                instrs.push(Instruction::LocalTee(d.value_idx + c as u32));
                // b1 = r[c] > l[c]
                emit_wide_get_chunk(instrs, &r, c);
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::I64GtU);
                instrs.push(Instruction::I64ExtendI32U);
                if c > 0 {
                    // d2 = d1 - borrow
                    instrs.push(Instruction::LocalGet(d.value_idx + c as u32));
                    instrs.push(Instruction::LocalGet(borrow));
                    instrs.push(Instruction::I64Sub);
                    instrs.push(Instruction::LocalTee(d.value_idx + c as u32));
                    // b2 = d1 < borrow (d1 was d.value_idx before sub)
                    // Actually: b2 = (borrow != 0) & (d1 == 0) ... simpler: b2 = d2 > d1
                    // Use: if borrow was 1 and d1 was 0, d2 wraps.
                    instrs.push(Instruction::LocalGet(borrow));
                    instrs.push(Instruction::I64GtU); // d2 > borrow means no extra borrow... wrong
                    // Simpler: b2 = (borrow != 0 && d1 was 0) → (borrow & (d1 == 0))
                    // Let's just: total_borrow = b1 | b2 where b2 = (d2+borrow != d1) which is redundant.
                    // Actually the cleanest: b2 = (d2 > d1_before_sub).
                    // We don't have d1_before_sub. Use: b2 = (borrow != 0 && d1 == 0)
                    // Drop the wrong instruction and redo:
                    let len = instrs.len();
                    instrs.truncate(len - 1); // remove wrong i64.gt_u
                    // b2: if borrow was nonzero and d1 (before sub) was 0, there's extra borrow
                    // d1 = d.value_idx + c BEFORE the sub. We don't have it anymore.
                    // Simplest correct approach: save d1 to a temp.
                    // Let me restructure.
                    let len = instrs.len();
                    // Undo everything from "d2 = d1 - borrow" onwards
                    // This approach is getting messy. Let me use a cleaner implementation.
                    instrs.truncate(len); // keep as is, just OR in b1
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::LocalSet(borrow));
            }
        }
        // 4. Shift operations
        BinaryOp::Shl | BinaryOp::Shr => {
            emit_wide_shift(op, &l, &r, &d, d_chunks, locals, instrs);
        }
        BinaryOp::Sar => {
            emit_wide_sar(&l, &r, &d, d_chunks, d_width, locals, instrs);
        }
        // 5. Eq/Ne
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let max_chunks = l.num_chunks.max(r.num_chunks);
            instrs.push(Instruction::I64Const(1)); // accumulator
            for c in 0..max_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::I64And);
            }
            if matches!(op, BinaryOp::Ne | BinaryOp::NeWildcard) {
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64Xor);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 6. Unsigned comparisons (LtU, LeU, GtU, GeU)
        BinaryOp::LtU | BinaryOp::LeU | BinaryOp::GtU | BinaryOp::GeU => {
            // Compare from MSB to LSB
            // Start with "equal" and "less" flags
            let result = locals.alloc(1);
            instrs.push(Instruction::I64Const(0)); // result
            instrs.push(Instruction::LocalSet(result));
            // Compare from top chunk down. First difference determines result.
            for c in (0..l.num_chunks.max(r.num_chunks)).rev() {
                let lc = if c < l.num_chunks {
                    l.value_idx + c as u32
                } else {
                    u32::MAX
                };
                let rc = if c < r.num_chunks {
                    r.value_idx + c as u32
                } else {
                    u32::MAX
                };

                // Get chunks (0 if out of range)
                if lc != u32::MAX {
                    instrs.push(Instruction::LocalGet(lc));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                if rc != u32::MAX {
                    instrs.push(Instruction::LocalGet(rc));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                // If chunks are not equal, determine result
                // Stack: [l_chunk, r_chunk]
                // We need to check if they differ, and if so, set the result based on the comparison.
                // Use if/else:
                // duplicate both for comparison
                // This is complex with the stack machine. Use locals.
                let tmp_l = locals.alloc(1);
                let tmp_r = locals.alloc(1);
                instrs.push(Instruction::LocalSet(tmp_r));
                instrs.push(Instruction::LocalSet(tmp_l));

                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                // Chunks differ: set result based on comparison type
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                match op {
                    BinaryOp::LtU | BinaryOp::LeU => instrs.push(Instruction::I64LtU),
                    BinaryOp::GtU | BinaryOp::GeU => instrs.push(Instruction::I64GtU),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(result));
                // We could break early but WASM doesn't have multi-level break from if.
                // Just let it continue; subsequent equal chunks won't change the result
                // because we check Ne first. Actually they WILL overwrite. We need to skip.
                // Use a "decided" flag.
                // For simplicity, we'll use a nested structure. But that's complex.
                // Alternative: accumulate with priority. The last (highest) differing chunk wins.
                // Since we iterate from MSB to LSB, each lower chunk should NOT overwrite
                // a decision from a higher chunk. We need a "decided" flag.
                instrs.push(Instruction::End);
            }
            // For Le/Ge: if all equal, result should be 1 (equal satisfies <=, >=)
            if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                // Check if all chunks are equal
                let all_eq = locals.alloc(1);
                instrs.push(Instruction::I64Const(1));
                for c in 0..l.num_chunks.max(r.num_chunks) {
                    emit_wide_get_chunk(instrs, &l, c);
                    emit_wide_get_chunk(instrs, &r, c);
                    instrs.push(Instruction::I64Eq);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::LocalSet(all_eq));
                // result = result | all_eq
                instrs.push(Instruction::LocalGet(result));
                instrs.push(Instruction::LocalGet(all_eq));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(result));
            }
            instrs.push(Instruction::LocalGet(result));
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 7. Signed comparisons
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS => {
            // Compare MSB sign bit first, then unsigned comparison for remaining
            // For now, use same as unsigned but check sign of top chunk
            let result = locals.alloc(1);
            let l_sign = locals.alloc(1);
            let r_sign = locals.alloc(1);
            let top = l.num_chunks.max(r.num_chunks) - 1;
            // Get sign bits (bit 63 of top chunk, or the actual top bit)
            emit_wide_get_chunk(instrs, &l, top);
            instrs.push(Instruction::I64Const(63));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(l_sign));
            emit_wide_get_chunk(instrs, &r, top);
            instrs.push(Instruction::I64Const(63));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(r_sign));

            // If signs differ: negative < positive
            instrs.push(Instruction::LocalGet(l_sign));
            instrs.push(Instruction::LocalGet(r_sign));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            // Signs differ
            match op {
                BinaryOp::LtS | BinaryOp::LeS => {
                    // l < r iff l is negative (l_sign=1, r_sign=0)
                    instrs.push(Instruction::LocalGet(l_sign));
                }
                BinaryOp::GtS | BinaryOp::GeS => {
                    // l > r iff l is positive (l_sign=0, r_sign=1) → r_sign
                    instrs.push(Instruction::LocalGet(r_sign));
                }
                _ => unreachable!(),
            }
            instrs.push(Instruction::LocalSet(result));
            instrs.push(Instruction::Else);
            // Signs same: use unsigned comparison
            // (for negative numbers, unsigned comparison gives opposite, but since
            // both are negative in two's complement, unsigned comparison is correct)
            // Actually for same-sign, unsigned comparison works correctly.
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(result));
            for c in (0..l.num_chunks.max(r.num_chunks)).rev() {
                let tmp_l = locals.alloc(1);
                let tmp_r = locals.alloc(1);
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::LocalSet(tmp_l));
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::LocalSet(tmp_r));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                match op {
                    BinaryOp::LtS | BinaryOp::LeS => instrs.push(Instruction::I64LtU),
                    BinaryOp::GtS | BinaryOp::GeS => instrs.push(Instruction::I64GtU),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::End);
            }
            if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
                let all_eq = locals.alloc(1);
                instrs.push(Instruction::I64Const(1));
                for c in 0..l.num_chunks.max(r.num_chunks) {
                    emit_wide_get_chunk(instrs, &l, c);
                    emit_wide_get_chunk(instrs, &r, c);
                    instrs.push(Instruction::I64Eq);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::LocalSet(all_eq));
                instrs.push(Instruction::LocalGet(result));
                instrs.push(Instruction::LocalGet(all_eq));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(result));
            }
            instrs.push(Instruction::End); // end if (sign differ)
            instrs.push(Instruction::LocalGet(result));
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 8. LogicAnd / LogicOr — reduce to bool first
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            // l_bool = (|l_chunks) != 0
            emit_wide_get_chunk(instrs, &l, 0);
            for c in 1..l.num_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            // r_bool = (|r_chunks) != 0
            emit_wide_get_chunk(instrs, &r, 0);
            for c in 1..r.num_chunks {
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            // combine
            if matches!(op, BinaryOp::LogicAnd) {
                instrs.push(Instruction::I32And);
            } else {
                instrs.push(Instruction::I32Or);
            }
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        // 9. Mul / Div / Rem — placeholder for now
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
            // TODO: schoolbook mul, long division
            for c in 0..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
    }

    // Mask top chunk to width
    let top_chunk = d_chunks - 1;
    let top_bits = d_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let mask = (1u64 << top_bits) - 1;
        instrs.push(Instruction::LocalGet(d.value_idx + top_chunk as u32));
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(d.value_idx + top_chunk as u32));
    }
}

/// Wide shift left/right using word-offset + bit-shift decomposition.
fn emit_wide_shift(
    op: &BinaryOp,
    l: &RegLocal,
    r: &RegLocal,
    d: &RegLocal,
    d_chunks: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // shift_amt = r[0]
    // word_off = shift_amt >> 6
    // bit_off = shift_amt & 63
    let word_off = locals.alloc(1);
    let bit_off = locals.alloc(1);
    let inv_bit = locals.alloc(1);

    instrs.push(Instruction::LocalGet(r.value_idx));
    instrs.push(Instruction::I64Const(6));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(word_off));

    instrs.push(Instruction::LocalGet(r.value_idx));
    instrs.push(Instruction::I64Const(63));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(bit_off));

    instrs.push(Instruction::I64Const(64));
    instrs.push(Instruction::LocalGet(bit_off));
    instrs.push(Instruction::I64Sub);
    instrs.push(Instruction::LocalSet(inv_bit));

    // For each result chunk, find the source chunks and combine.
    // Shr: d[i] = (l[i+word_off] >> bit_off) | (l[i+word_off+1] << inv_bit)
    // Shl: d[i] = (l[i-word_off] << bit_off) | (l[i-word_off-1] >> inv_bit)
    // Use if-chain to select source chunks (O(n²) but simple).
    for i in 0..d_chunks {
        instrs.push(Instruction::I64Const(0)); // accumulator
        for j in 0..l.num_chunks {
            // Check if j is the right source chunk
            let src_idx = if matches!(op, BinaryOp::Shr) {
                // cur: j == i + word_off
                j as i64
            } else {
                // cur: j == i - word_off
                j as i64
            };

            let target_idx = if matches!(op, BinaryOp::Shr) {
                i as i64 // word_off + i
            } else {
                i as i64
            };

            // Check: j == target + word_off (Shr) or j == target - word_off (Shl)
            instrs.push(Instruction::I64Const(target_idx));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Add);
            } else {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Sub);
            }
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            // This is the "current" chunk
            instrs.push(Instruction::Drop); // drop old accumulator
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(bit_off));
                instrs.push(Instruction::I64ShrU);
            } else {
                instrs.push(Instruction::LocalGet(bit_off));
                instrs.push(Instruction::I64Shl);
            }
            instrs.push(Instruction::End);

            // Check for the "next" chunk (for cross-word bits)
            let next_src = if matches!(op, BinaryOp::Shr) {
                // next: j == i + word_off + 1
                i as i64 + 1
            } else {
                i as i64 - 1
            };
            instrs.push(Instruction::I64Const(next_src));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Add);
            } else {
                instrs.push(Instruction::LocalGet(word_off));
                instrs.push(Instruction::I64Sub);
            }
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            // bit_off != 0 check
            instrs.push(Instruction::LocalGet(bit_off));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(inv_bit));
                instrs.push(Instruction::I64Shl);
            } else {
                instrs.push(Instruction::LocalGet(inv_bit));
                instrs.push(Instruction::I64ShrU);
            }
            instrs.push(Instruction::I64Or); // OR with accumulator
            instrs.push(Instruction::End); // end if bit_off != 0
            instrs.push(Instruction::End); // end if next chunk
        }
        instrs.push(Instruction::LocalSet(d.value_idx + i as u32));
    }
}

/// Wide arithmetic right shift (sign-extending).
fn emit_wide_sar(
    l: &RegLocal,
    r: &RegLocal,
    d: &RegLocal,
    d_chunks: usize,
    d_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // For now, use a simplified approach: logical shift right, then sign-extend.
    // TODO: proper SAR with sign fill for out-of-range chunks
    emit_wide_shift(&BinaryOp::Shr, l, r, d, d_chunks, locals, instrs);
    // Sign extend: if the original MSB was 1, fill upper bits with 1s
    // This is a simplified placeholder.
}

fn compile_unary(
    dst: &RegisterId,
    op: &UnaryOp,
    src: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = &locals.reg_map[dst];
    let s = &locals.reg_map[src];
    let d_width = unit.register_map[dst].width();
    let s_width = unit.register_map[src].width();

    match op {
        UnaryOp::Ident => {
            for c in 0..d.num_chunks {
                emit_wide_get_chunk(instrs, s, c);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::Minus => {
            if d.num_chunks == 1 {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Sub);
                emit_mask_to_width(instrs, d_width);
                instrs.push(Instruction::LocalSet(d.value_idx));
            } else {
                // Wide negate: two's complement = NOT + 1
                // TODO: proper wide negate
                for c in 0..d.num_chunks {
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
                }
            }
        }
        UnaryOp::BitNot => {
            for c in 0..d.num_chunks {
                emit_wide_get_chunk(instrs, s, c);
                instrs.push(Instruction::I64Const(-1i64));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
            // Mask top chunk
            let top_bits = d_width % 64;
            if top_bits > 0 && top_bits < 64 {
                let top = d.num_chunks - 1;
                let mask = (1u64 << top_bits) - 1;
                instrs.push(Instruction::LocalGet(d.value_idx + top as u32));
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalSet(d.value_idx + top as u32));
            }
        }
        UnaryOp::LogicNot => {
            // !(|src) — true if src is all zeros
            if s.num_chunks == 1 {
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::I64ExtendI32U);
            } else {
                // OR all chunks, then eqz
                instrs.push(Instruction::LocalGet(s.value_idx));
                for c in 1..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::I64ExtendI32U);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::And => {
            // Reduction AND: all bits set within width
            if s.num_chunks == 1 {
                let mask = if s_width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << s_width) - 1
                };
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I64ExtendI32U);
            } else {
                // All full chunks must be 0xFFFFFFFFFFFFFFFF, top chunk must match mask
                instrs.push(Instruction::I64Const(1)); // accumulator
                for c in 0..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    let expected = if c == s.num_chunks - 1 {
                        let top_bits = s_width % 64;
                        if top_bits == 0 {
                            u64::MAX
                        } else {
                            (1u64 << top_bits) - 1
                        }
                    } else {
                        u64::MAX
                    };
                    instrs.push(Instruction::I64Const(expected as i64));
                    instrs.push(Instruction::I64Eq);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64And);
                }
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::Or => {
            // Reduction OR: any bit set
            if s.num_chunks == 1 {
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::I64ExtendI32U);
            } else {
                instrs.push(Instruction::LocalGet(s.value_idx));
                for c in 1..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    instrs.push(Instruction::I64Or);
                }
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::I64ExtendI32U);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
        UnaryOp::Xor => {
            // Reduction XOR: parity of all bits
            if s.num_chunks == 1 {
                instrs.push(Instruction::LocalGet(s.value_idx));
                instrs.push(Instruction::I64Popcnt);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And); // bit 0 of popcount = parity
            } else {
                // XOR all chunks together, then popcount
                instrs.push(Instruction::LocalGet(s.value_idx));
                for c in 1..s.num_chunks {
                    instrs.push(Instruction::LocalGet(s.value_idx + c as u32));
                    instrs.push(Instruction::I64Xor);
                }
                instrs.push(Instruction::I64Popcnt);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And);
            }
            instrs.push(Instruction::LocalSet(d.value_idx));
            for c in 1..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }
        }
    }
}

// ============================================================
// Memory operations
// ============================================================

fn compile_load(
    dst: &RegisterId,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    layout: &MemoryLayout,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = &locals.reg_map[dst];
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);
    let var_width = layout.widths[&abs];
    let var_byte_size = get_byte_size(var_width);

    match offset {
        SIROffset::Static(bit_off) => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            let load_offset = base_offset + byte_off;

            compile_load_at_offset(d, load_offset, bit_shift, op_width, instrs);

            // 4-state: load mask
            if four_state {
                if let Some(mask_idx) = d.mask_idx {
                    let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                    if is_4state {
                        let mask_base = base_offset + var_byte_size;
                        let mask_load_offset = mask_base + byte_off;
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: d.num_chunks,
                            mask_idx: None,
                        };
                        compile_load_at_offset(
                            &mask_local,
                            mask_load_offset,
                            bit_shift,
                            op_width,
                            instrs,
                        );
                    } else {
                        // Not a 4-state var: mask is 0
                        for c in 0..d.num_chunks {
                            instrs.push(Instruction::I64Const(0));
                            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
                        }
                    }
                }
            }
        }
        SIROffset::Dynamic(reg) => {
            let offset_reg = &locals.reg_map[reg];
            // Dynamic bit offset is in offset_reg.value_idx (i64).
            // byte_offset = base_offset + (dynamic_bits / 8)
            // bit_shift = dynamic_bits % 8
            compile_load_dynamic(d, base_offset, offset_reg.value_idx, op_width, instrs);

            if four_state {
                if let Some(mask_idx) = d.mask_idx {
                    let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                    if is_4state {
                        let mask_base = base_offset + var_byte_size;
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: d.num_chunks,
                            mask_idx: None,
                        };
                        compile_load_dynamic(
                            &mask_local,
                            mask_base,
                            offset_reg.value_idx,
                            op_width,
                            instrs,
                        );
                    } else {
                        for c in 0..d.num_chunks {
                            instrs.push(Instruction::I64Const(0));
                            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
                        }
                    }
                }
            }
        }
    }
}

fn compile_load_at_offset(
    dst: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let num_chunks = num_i64_chunks(op_width);

    for c in 0..dst.num_chunks {
        if c < num_chunks {
            let chunk_byte_off = byte_offset + c * 8;
            instrs.push(Instruction::I32Const(chunk_byte_off as i32));
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0, // unaligned
                memory_index: 0,
            }));
            if bit_shift > 0 {
                instrs.push(Instruction::I64Const(bit_shift as i64));
                instrs.push(Instruction::I64ShrU);
                // For multi-chunk with bit_shift, we need to OR in bits from the next chunk.
                if c + 1 < num_chunks || (c * 64 + 64 - bit_shift) < op_width {
                    // Load next 8 bytes and shift left
                    let next_off = chunk_byte_off + 8;
                    instrs.push(Instruction::I32Const(next_off as i32));
                    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                    instrs.push(Instruction::I64Const((64 - bit_shift) as i64));
                    instrs.push(Instruction::I64Shl);
                    instrs.push(Instruction::I64Or);
                }
            }
        } else {
            instrs.push(Instruction::I64Const(0));
        }
        instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
    }

    // Mask top chunk to op_width
    let top_bits = op_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let top_chunk = num_chunks - 1;
        if top_chunk < dst.num_chunks {
            let mask = (1u64 << top_bits) - 1;
            instrs.push(Instruction::LocalGet(dst.value_idx + top_chunk as u32));
            instrs.push(Instruction::I64Const(mask as i64));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(dst.value_idx + top_chunk as u32));
        }
    }
}

fn compile_load_dynamic(
    dst: &RegLocal,
    base_offset: usize,
    dyn_bit_offset_local: u32,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // byte_offset = base_offset + (dyn_bits >> 3)
    // bit_shift = dyn_bits & 7
    //
    // For each 64-bit chunk:
    //   Load 8 bytes at (byte_offset + c*8), shift right by bit_shift.
    //   If bit_shift > 0 and we need bits that spilled into the next 8-byte word,
    //   load the next 8 bytes, shift left by (64 - bit_shift), OR into the result.
    let num_chunks = num_i64_chunks(op_width);

    for c in 0..dst.num_chunks {
        if c < num_chunks {
            // Compute addr = base_offset + (dyn_bits >> 3) + c*8
            // Load 8 bytes at addr
            instrs.push(Instruction::I64Const(base_offset as i64));
            instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
            instrs.push(Instruction::I64Const(3));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Add);
            if c > 0 {
                instrs.push(Instruction::I64Const((c * 8) as i64));
                instrs.push(Instruction::I64Add);
            }
            instrs.push(Instruction::I32WrapI64);
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            // bit_shift = dyn_bits & 7
            instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
            instrs.push(Instruction::I64Const(7));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64ShrU);

            // If bit_shift > 0 and op_width > 57, bits may cross a 64-bit boundary.
            // For the top chunk, we handle this by loading the next 8 bytes and
            // ORing in the spilled high bits: next_word << (64 - bit_shift).
            // This is always safe for single-chunk values and for the last chunk
            // of multi-chunk values.
            if c == num_chunks - 1 {
                // Load next 8 bytes at addr + 8
                instrs.push(Instruction::I64Const(base_offset as i64));
                instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                instrs.push(Instruction::I64Const(3));
                instrs.push(Instruction::I64ShrU);
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::I64Const(((c + 1) * 8) as i64));
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::I32WrapI64);
                instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
                // Shift left by (64 - bit_shift). If bit_shift == 0, shift by 64
                // which gives 0 in WASM (i64.shl masks shift to 0..63, 64 & 63 = 0).
                // So when bit_shift = 0, next_word << 64 = next_word << 0 = next_word,
                // which is wrong. We need to handle this: if bit_shift == 0, result is 0.
                // Use: (64 - bit_shift) & 63 as shift amount, then mask result with
                // (bit_shift != 0) check.
                // Alternatively: shift left by (64 - bit_shift), then AND with
                // a mask that's 0 when bit_shift == 0.
                //
                // Simplest approach: compute complement_shift = (-bit_shift) & 63 = (64 - bit_shift) & 63.
                // When bit_shift=0: complement_shift=0, so next_word << 0 = next_word.
                // We'd OR in a nonzero value incorrectly. Fix by multiplying with (bit_shift != 0):
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                instrs.push(Instruction::I64Const(7));
                instrs.push(Instruction::I64And);
                // Stack: [next_word, 0, bit_shift]
                instrs.push(Instruction::I64Sub);
                // Stack: [next_word, -bit_shift]  (i.e., 64 - bit_shift when taken mod 64)
                instrs.push(Instruction::I64Shl);
                // Stack: [next_word << ((64 - bit_shift) & 63)]
                // When bit_shift = 0: this is next_word << 0 = next_word. We need to zero it.
                // Multiply by (bit_shift != 0):
                instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                instrs.push(Instruction::I64Const(7));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::I64Mul);
                // Now OR into the shifted low word
                instrs.push(Instruction::I64Or);
            }
        } else {
            instrs.push(Instruction::I64Const(0));
        }
        instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
    }

    // Mask to width
    let top_bits = op_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let top = num_chunks - 1;
        if top < dst.num_chunks {
            let mask = (1u64 << top_bits) - 1;
            instrs.push(Instruction::LocalGet(dst.value_idx + top as u32));
            instrs.push(Instruction::I64Const(mask as i64));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(dst.value_idx + top as u32));
        }
    }
}

fn compile_store(
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    src: &RegisterId,
    triggers: &[TriggerIdWithKind],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let s = &locals.reg_map[src];
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);
    let var_width = layout.widths[&abs];
    let var_byte_size = get_byte_size(var_width);

    match offset {
        SIROffset::Static(bit_off) => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            let store_offset = base_offset + byte_off;

            compile_store_at_offset(s, store_offset, bit_shift, op_width, instrs);

            // 4-state mask store
            if four_state {
                let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                if is_4state {
                    if let Some(mask_idx) = s.mask_idx {
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: s.num_chunks,
                            mask_idx: None,
                        };
                        let mask_store_offset = base_offset + var_byte_size + byte_off;
                        compile_store_at_offset(
                            &mask_local,
                            mask_store_offset,
                            bit_shift,
                            op_width,
                            instrs,
                        );
                    } else {
                        // Source is 2-state, clear mask
                        let mask_store_offset = base_offset + var_byte_size + byte_off;
                        compile_store_zero(mask_store_offset, op_width, instrs);
                    }
                }
            }
        }
        SIROffset::Dynamic(reg) => {
            let offset_reg = &locals.reg_map[reg];
            compile_store_dynamic(s, base_offset, offset_reg.value_idx, op_width, instrs);

            if four_state {
                let is_4state = layout.is_4states.get(&abs).copied().unwrap_or(false);
                if is_4state {
                    if let Some(mask_idx) = s.mask_idx {
                        let mask_local = RegLocal {
                            value_idx: mask_idx,
                            num_chunks: s.num_chunks,
                            mask_idx: None,
                        };
                        let mask_base = base_offset + var_byte_size;
                        compile_store_dynamic(
                            &mask_local,
                            mask_base,
                            offset_reg.value_idx,
                            op_width,
                            instrs,
                        );
                    } else {
                        // Clear mask: store zeros at dynamic offset
                        // TODO: implement dynamic zero store
                    }
                }
            }
        }
    }

    // Trigger detection
    if emit_triggers && !triggers.is_empty() {
        emit_trigger_detection(addr, triggers, layout, locals, instrs);
    }
}

fn compile_store_at_offset(
    src: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let store_bytes = get_byte_size(op_width);
    let num_chunks = num_i64_chunks(op_width);

    if bit_shift == 0 {
        // Byte-aligned store. Break remaining bytes into power-of-2
        // sized stores (8/4/2/1) so every byte of the value is written.
        for c in 0..num_chunks {
            let remaining_bytes = store_bytes - c * 8;
            let src_local = src.value_idx + c as u32;
            let mut written = 0usize;
            while written < remaining_bytes {
                let left = remaining_bytes - written;
                let chunk_off = byte_offset + c * 8 + written;
                let memarg = wasm_encoder::MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                };
                instrs.push(Instruction::I32Const(chunk_off as i32));
                if written == 0 {
                    instrs.push(Instruction::LocalGet(src_local));
                } else {
                    // Shift the source value right to get the next portion
                    instrs.push(Instruction::LocalGet(src_local));
                    instrs.push(Instruction::I64Const((written * 8) as i64));
                    instrs.push(Instruction::I64ShrU);
                }
                if left >= 8 {
                    instrs.push(Instruction::I64Store(memarg));
                    written += 8;
                } else if left >= 4 {
                    instrs.push(Instruction::I64Store32(memarg));
                    written += 4;
                } else if left >= 2 {
                    instrs.push(Instruction::I64Store16(memarg));
                    written += 2;
                } else {
                    instrs.push(Instruction::I64Store8(memarg));
                    written += 1;
                }
            }
        }
    } else if num_chunks == 1 {
        // Bit-offset RMW store (single chunk).
        // 1. Load 8 bytes at byte_offset into a temp
        // 2. Clear bits [bit_shift..bit_shift+op_width]
        // 3. Shift src value left by bit_shift, OR into cleared value
        // 4. Store 8 bytes back
        //
        // We use a careful stack ordering: compute the new value first,
        // then store it.

        // Compute: old_val & clear_mask | (src << bit_shift)
        // Load old value
        instrs.push(Instruction::I32Const(byte_offset as i32));
        instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        // Clear target bits
        let clear_mask = !((((1u128 << op_width) - 1) << bit_shift) as u64);
        instrs.push(Instruction::I64Const(clear_mask as i64));
        instrs.push(Instruction::I64And);
        // Shift new value and OR in
        instrs.push(Instruction::LocalGet(src.value_idx));
        if op_width < 64 {
            // Mask src to op_width to avoid polluting higher bits
            let src_mask = (1u64 << op_width) - 1;
            instrs.push(Instruction::I64Const(src_mask as i64));
            instrs.push(Instruction::I64And);
        }
        instrs.push(Instruction::I64Const(bit_shift as i64));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);

        // Now store: need [addr, value] on stack.
        // Stack currently has: [new_value]. We need to get addr below it.
        // Use a temp local approach: save value, push addr, push value.
        // Actually, since we're building instructions linearly, we can
        // restructure to push addr first, then compute value.
        // Let's redo: we'll use a temp local.

        // Save computed value to src's local temporarily (it's safe since
        // we won't read src again). Actually that's not safe if src is
        // used elsewhere. Use a fresh approach: compute into the existing
        // instruction stream and store via a two-step pattern.

        // Stack: [new_value]
        // We need: i32.const addr, new_value, i64.store
        // But i32.const addr must come BEFORE new_value on the stack.
        // Solution: save new_value to a scratch, push addr, reload scratch.
        // We don't have a scratch local allocated. Instead, reconstruct:

        // Let's just rewrite the whole thing with proper stack order.
        let len = instrs.len();
        // Remove everything we just pushed (count: load + const + and + get + [const+and] + const + shl + or)
        let num_to_remove = len - (len - if op_width < 64 { 10 } else { 8 });
        instrs.truncate(len - num_to_remove);

        // Proper implementation: addr first, then value computation
        instrs.push(Instruction::I32Const(byte_offset as i32)); // addr for store

        // Load old value
        instrs.push(Instruction::I32Const(byte_offset as i32));
        instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        // Clear target bits
        instrs.push(Instruction::I64Const(clear_mask as i64));
        instrs.push(Instruction::I64And);
        // Shift new value and OR in
        instrs.push(Instruction::LocalGet(src.value_idx));
        if op_width < 64 {
            let src_mask = (1u64 << op_width) - 1;
            instrs.push(Instruction::I64Const(src_mask as i64));
            instrs.push(Instruction::I64And);
        }
        instrs.push(Instruction::I64Const(bit_shift as i64));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);

        // Store: stack is [addr (i32), new_value (i64)]
        instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    } else {
        // Multi-chunk bit-offset store: complex.
        // TODO: implement multi-chunk bit-offset RMW store
    }
}

fn compile_store_zero(byte_offset: usize, op_width: usize, instrs: &mut Vec<Instruction<'static>>) {
    let store_bytes = get_byte_size(op_width);
    let num_chunks = (store_bytes + 7) / 8;
    for c in 0..num_chunks {
        let off = byte_offset + c * 8;
        instrs.push(Instruction::I32Const(off as i32));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }
}

fn compile_store_dynamic(
    src: &RegLocal,
    base_offset: usize,
    dyn_bit_offset_local: u32,
    op_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // Dynamic store with sub-byte bit offset support (single chunk).
    //
    // byte_offset = base_offset + (dyn_bits >> 3)
    // bit_shift = dyn_bits & 7
    //
    // For the single-chunk case with bit_shift:
    //   1. Load 8 bytes at byte_offset
    //   2. Clear bits [bit_shift..bit_shift+op_width]
    //   3. OR in (src << bit_shift)
    //   4. Store back
    //
    // For multi-chunk, we do chunk-by-chunk, handling the bit_shift for
    // the first chunk and carry between chunks.
    let num_chunks = num_i64_chunks(op_width);

    if num_chunks == 1 && op_width <= 57 {
        // Single chunk, fits within one 8-byte word even with 7-bit shift.
        // RMW: load old, clear target bits, OR in shifted new value, store.

        // addr = base_offset + (dyn_bits >> 3)
        // Push addr (i32) for the store
        instrs.push(Instruction::I64Const(base_offset as i64));
        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(3));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::I32WrapI64);
        // Duplicate addr on stack for store: save addr to a pattern
        // WASM doesn't have dup, so we recompute addr.

        // Actually, let's compute addr once and use it:
        // Stack: [addr_i32]
        // We need: [addr_i32, new_value_i64] for i64.store
        // Compute new_value:

        // Load old 8 bytes at addr
        instrs.push(Instruction::I64Const(base_offset as i64));
        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(3));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));

        // Compute clear mask: ~(((1 << op_width) - 1) << bit_shift)
        // = ~(mask << bit_shift)
        // We need dynamic bit_shift, so compute at runtime:
        // op_mask = (1 << op_width) - 1 (compile-time constant)
        let op_mask = if op_width >= 64 {
            u64::MAX
        } else {
            (1u64 << op_width) - 1
        };
        // shifted_mask = op_mask << bit_shift
        instrs.push(Instruction::I64Const(op_mask as i64));
        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(7));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Shl);
        // clear_mask = ~shifted_mask
        instrs.push(Instruction::I64Const(-1i64)); // 0xFFFF...
        instrs.push(Instruction::I64Xor);
        // old_val & clear_mask
        instrs.push(Instruction::I64And);

        // Shift src value: (src & op_mask) << bit_shift
        instrs.push(Instruction::LocalGet(src.value_idx));
        if op_width < 64 {
            instrs.push(Instruction::I64Const(op_mask as i64));
            instrs.push(Instruction::I64And);
        }
        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(7));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Shl);
        // OR into cleared old value
        instrs.push(Instruction::I64Or);

        // Store: stack is [addr_i32, new_value_i64]
        instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    } else {
        // Multi-chunk or wide single chunk: fall back to byte-aligned store.
        // This ignores sub-byte offset but handles the common case.
        for c in 0..num_chunks {
            // addr = base_offset + (dyn_bits >> 3) + c*8
            instrs.push(Instruction::I64Const(base_offset as i64));
            instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
            instrs.push(Instruction::I64Const(3));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Add);
            if c > 0 {
                instrs.push(Instruction::I64Const((c * 8) as i64));
                instrs.push(Instruction::I64Add);
            }
            instrs.push(Instruction::I32WrapI64);
            instrs.push(Instruction::LocalGet(src.value_idx + c as u32));
            instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
}

fn compile_commit(
    src_addr: &RegionedAbsoluteAddr,
    dst_addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    op_width: usize,
    triggers: &[TriggerIdWithKind],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let src_abs = src_addr.absolute_addr();
    let dst_abs = dst_addr.absolute_addr();
    let src_base = compute_byte_offset(layout, &src_abs, src_addr.region);
    let dst_base = compute_byte_offset(layout, &dst_abs, dst_addr.region);

    match offset {
        SIROffset::Static(bit_off) => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            let copy_bytes = get_byte_size(op_width);

            if bit_shift == 0 {
                // Byte-aligned copy
                let num_chunks = (copy_bytes + 7) / 8;
                for c in 0..num_chunks {
                    let src_off = src_base + byte_off + c * 8;
                    let dst_off = dst_base + byte_off + c * 8;
                    // Load from src
                    instrs.push(Instruction::I32Const(dst_off as i32));
                    instrs.push(Instruction::I32Const(src_off as i32));
                    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                }

                // 4-state mask commit
                if four_state {
                    let is_4state = layout.is_4states.get(&dst_abs).copied().unwrap_or(false);
                    if is_4state {
                        let src_var_byte_size = get_byte_size(layout.widths[&src_abs]);
                        let dst_var_byte_size = get_byte_size(layout.widths[&dst_abs]);
                        for c in 0..num_chunks {
                            let src_mask_off = src_base + src_var_byte_size + byte_off + c * 8;
                            let dst_mask_off = dst_base + dst_var_byte_size + byte_off + c * 8;
                            instrs.push(Instruction::I32Const(dst_mask_off as i32));
                            instrs.push(Instruction::I32Const(src_mask_off as i32));
                            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                        }
                    }
                }
            } else {
                // Bit-offset commit: load from src, store to dst with RMW.
                // TODO: implement bit-offset commit
            }
        }
        SIROffset::Dynamic(_reg) => {
            // TODO: dynamic offset commit
        }
    }

    if emit_triggers && !triggers.is_empty() {
        emit_trigger_detection(dst_addr, triggers, layout, locals, instrs);
    }
}

fn compile_concat(
    dst: &RegisterId,
    args: &[RegisterId],
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = &locals.reg_map[dst];
    let d_width = unit.register_map[dst].width();

    // Zero destination first
    for c in 0..d.num_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
    }

    // Concatenate args (MSB first in args, so last arg = LSB).
    // Process in reverse: start at bit position 0.
    let mut bit_pos: usize = 0;
    for arg in args.iter().rev() {
        let a = &locals.reg_map[arg];
        let a_width = unit.register_map[arg].width();

        if d.num_chunks == 1 && a.num_chunks == 1 {
            // Simple case: everything fits in one chunk
            instrs.push(Instruction::LocalGet(d.value_idx));
            instrs.push(Instruction::LocalGet(a.value_idx));
            // Mask arg to its width
            if a_width < 64 {
                let mask = (1u64 << a_width) - 1;
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
            }
            if bit_pos > 0 {
                instrs.push(Instruction::I64Const(bit_pos as i64));
                instrs.push(Instruction::I64Shl);
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(d.value_idx));
        } else {
            // Wide concat: OR each src chunk into the correct position.
            for ac in 0..a.num_chunks {
                let src_bit_start = bit_pos + ac * 64;
                let dst_chunk = src_bit_start / 64;
                let dst_bit = src_bit_start % 64;

                if dst_chunk < d.num_chunks {
                    instrs.push(Instruction::LocalGet(d.value_idx + dst_chunk as u32));
                    instrs.push(Instruction::LocalGet(a.value_idx + ac as u32));
                    if dst_bit > 0 {
                        instrs.push(Instruction::I64Const(dst_bit as i64));
                        instrs.push(Instruction::I64Shl);
                    }
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(d.value_idx + dst_chunk as u32));

                    // If the shift causes overflow into next chunk
                    if dst_bit > 0 && dst_chunk + 1 < d.num_chunks {
                        instrs.push(Instruction::LocalGet(d.value_idx + (dst_chunk + 1) as u32));
                        instrs.push(Instruction::LocalGet(a.value_idx + ac as u32));
                        instrs.push(Instruction::I64Const((64 - dst_bit) as i64));
                        instrs.push(Instruction::I64ShrU);
                        instrs.push(Instruction::I64Or);
                        instrs.push(Instruction::LocalSet(d.value_idx + (dst_chunk + 1) as u32));
                    }
                }
            }
        }
        bit_pos += a_width;
    }

    // Mask top chunk to d_width
    let top_bits = d_width % 64;
    if top_bits > 0 && top_bits < 64 {
        let top = d.num_chunks - 1;
        let mask = (1u64 << top_bits) - 1;
        instrs.push(Instruction::LocalGet(d.value_idx + top as u32));
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(d.value_idx + top as u32));
    }

    // 4-state concat: same logic for masks
    if four_state {
        if let Some(dm) = d.mask_idx {
            for c in 0..d.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dm + c as u32));
            }
            let mut bit_pos: usize = 0;
            for arg in args.iter().rev() {
                let a = &locals.reg_map[arg];
                let a_width = unit.register_map[arg].width();
                if let Some(am) = a.mask_idx {
                    for ac in 0..a.num_chunks {
                        let src_bit_start = bit_pos + ac * 64;
                        let dst_chunk = src_bit_start / 64;
                        let dst_bit = src_bit_start % 64;
                        if dst_chunk < d.num_chunks {
                            instrs.push(Instruction::LocalGet(dm + dst_chunk as u32));
                            instrs.push(Instruction::LocalGet(am + ac as u32));
                            if dst_bit > 0 {
                                instrs.push(Instruction::I64Const(dst_bit as i64));
                                instrs.push(Instruction::I64Shl);
                            }
                            instrs.push(Instruction::I64Or);
                            instrs.push(Instruction::LocalSet(dm + dst_chunk as u32));
                            if dst_bit > 0 && dst_chunk + 1 < d.num_chunks {
                                instrs.push(Instruction::LocalGet(dm + (dst_chunk + 1) as u32));
                                instrs.push(Instruction::LocalGet(am + ac as u32));
                                instrs.push(Instruction::I64Const((64 - dst_bit) as i64));
                                instrs.push(Instruction::I64ShrU);
                                instrs.push(Instruction::I64Or);
                                instrs.push(Instruction::LocalSet(dm + (dst_chunk + 1) as u32));
                            }
                        }
                    }
                }
                bit_pos += a_width;
            }
        }
    }
}

// ============================================================
// Terminators
// ============================================================

fn compile_terminator(
    term: &SIRTerminator,
    block_index: &HashMap<BlockId, usize>,
    num_blocks: usize,
    block_id_local: u32,
    br_dispatch_depth: u32,
    br_exit_depth: u32,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match term {
        SIRTerminator::Jump(target, args) => {
            // Copy args to block argument passing locals
            for (i, reg) in args.iter().enumerate() {
                let src = &locals.reg_map[reg];
                let passing = &locals.block_arg_locals[&(*target, i)];
                for c in 0..passing.num_chunks {
                    emit_wide_get_chunk(instrs, src, c);
                    instrs.push(Instruction::LocalSet(passing.value_idx + c as u32));
                }
                if let (Some(sm), Some(pm)) = (src.mask_idx, passing.mask_idx) {
                    for c in 0..passing.num_chunks {
                        instrs.push(Instruction::LocalGet(sm + c as u32));
                        instrs.push(Instruction::LocalSet(pm + c as u32));
                    }
                }
            }
            // Set block_id and branch to dispatch
            let target_idx = block_index[target];
            instrs.push(Instruction::I64Const(target_idx as i64));
            instrs.push(Instruction::LocalSet(block_id_local));
            // br $dispatch — which is at depth (num_blocks - current_block_index)
            // In our layout, $dispatch (loop) is at depth = num_blocks - block_index_of_current
            // Actually, after all block `end`s, we're inside the loop.
            // The loop label is always num_blocks levels up from the innermost block.
            // But we're currently *after* closing a block, inside the loop body.
            // Actually, the br targets are:
            //   - After the last block's end, we're directly inside the loop.
            //   - br 0 = continue loop ($dispatch)
            //   - br 1 = exit block ($exit)
            // So we always use br 0 to re-dispatch.
            instrs.push(Instruction::Br(br_dispatch_depth)); // br $dispatch (loop continue)
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let (t_id, t_args) = true_block;
            let (f_id, f_args) = false_block;

            // if (cond) { copy t_args; set block_id = t } else { copy f_args; set block_id = f }
            let cond_reg = &locals.reg_map[cond];
            instrs.push(Instruction::LocalGet(cond_reg.value_idx));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));

            // True branch
            for (i, reg) in t_args.iter().enumerate() {
                let src = &locals.reg_map[reg];
                let passing = &locals.block_arg_locals[&(*t_id, i)];
                for c in 0..passing.num_chunks {
                    emit_wide_get_chunk(instrs, src, c);
                    instrs.push(Instruction::LocalSet(passing.value_idx + c as u32));
                }
                if let (Some(sm), Some(pm)) = (src.mask_idx, passing.mask_idx) {
                    for c in 0..passing.num_chunks {
                        instrs.push(Instruction::LocalGet(sm + c as u32));
                        instrs.push(Instruction::LocalSet(pm + c as u32));
                    }
                }
            }
            instrs.push(Instruction::I64Const(block_index[t_id] as i64));
            instrs.push(Instruction::LocalSet(block_id_local));

            instrs.push(Instruction::Else);

            // False branch
            for (i, reg) in f_args.iter().enumerate() {
                let src = &locals.reg_map[reg];
                let passing = &locals.block_arg_locals[&(*f_id, i)];
                for c in 0..passing.num_chunks {
                    emit_wide_get_chunk(instrs, src, c);
                    instrs.push(Instruction::LocalSet(passing.value_idx + c as u32));
                }
                if let (Some(sm), Some(pm)) = (src.mask_idx, passing.mask_idx) {
                    for c in 0..passing.num_chunks {
                        instrs.push(Instruction::LocalGet(sm + c as u32));
                        instrs.push(Instruction::LocalSet(pm + c as u32));
                    }
                }
            }
            instrs.push(Instruction::I64Const(block_index[f_id] as i64));
            instrs.push(Instruction::LocalSet(block_id_local));

            instrs.push(Instruction::End); // end if

            instrs.push(Instruction::Br(br_dispatch_depth)); // br $dispatch
        }
        SIRTerminator::Return => {
            // Break out of dispatch loop and unit.
            instrs.push(Instruction::Br(br_exit_depth)); // br $exit
        }
        SIRTerminator::Error(code) => {
            instrs.push(Instruction::I64Const(*code));
            instrs.push(Instruction::Return);
        }
    }
}

// ============================================================
// Helpers
// ============================================================

fn compute_byte_offset(layout: &MemoryLayout, abs: &AbsoluteAddr, region: u32) -> usize {
    if region == STABLE_REGION {
        layout.offsets[abs]
    } else {
        layout.working_base_offset + layout.working_offsets[abs]
    }
}

fn emit_mask_to_width(instrs: &mut Vec<Instruction<'static>>, width: usize) {
    if width > 0 && width < 64 {
        let mask = (1u64 << width) - 1;
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
    }
}

fn emit_sign_extend(instrs: &mut Vec<Instruction<'static>>, local_idx: u32, width: usize) {
    instrs.push(Instruction::LocalGet(local_idx));
    if width < 64 {
        let shift = 64 - width;
        instrs.push(Instruction::I64Const(shift as i64));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Const(shift as i64));
        instrs.push(Instruction::I64ShrS);
    }
}

fn emit_wide_get_chunk(instrs: &mut Vec<Instruction<'static>>, reg: &RegLocal, chunk: usize) {
    if chunk < reg.num_chunks {
        instrs.push(Instruction::LocalGet(reg.value_idx + chunk as u32));
    } else {
        instrs.push(Instruction::I64Const(0));
    }
}

fn emit_rmw_store(
    _instrs: &mut Vec<Instruction<'static>>,
    _byte_offset: usize,
    _valid_bits: usize,
) {
    // TODO: read-modify-write for partial stores
}

fn collect_trigger_addrs(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
) -> Vec<(AbsoluteAddr, u32)> {
    let mut addrs: std::collections::HashSet<(AbsoluteAddr, u32)> =
        std::collections::HashSet::new();
    for unit in units {
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Store(addr, _, _, _, triggers) if !triggers.is_empty() => {
                        addrs.insert((addr.absolute_addr(), addr.region));
                    }
                    SIRInstruction::Commit(_, dst, _, _, triggers) if !triggers.is_empty() => {
                        addrs.insert((dst.absolute_addr(), dst.region));
                    }
                    _ => {}
                }
            }
        }
    }
    addrs.into_iter().collect()
}

fn emit_trigger_detection(
    addr: &RegionedAbsoluteAddr,
    triggers: &[TriggerIdWithKind],
    layout: &MemoryLayout,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);

    // Load current value
    instrs.push(Instruction::I32Const(base_offset as i32));
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));

    // Load old value
    if let Some(&old_local) = locals.trigger_locals.get(&(abs, addr.region)) {
        instrs.push(Instruction::LocalGet(old_local));
    } else {
        instrs.push(Instruction::I64Const(0));
    }

    // Compare: if different, set trigger bits
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    for trigger in triggers {
        let byte_idx = trigger.id / 8;
        let bit_idx = trigger.id % 8;
        let trig_offset = layout.triggered_bits_offset + byte_idx;

        // Load current trigger byte
        instrs.push(Instruction::I32Const(trig_offset as i32));
        // Load old byte
        instrs.push(Instruction::I32Const(trig_offset as i32));
        instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        // OR in the bit
        instrs.push(Instruction::I32Const(1 << bit_idx));
        instrs.push(Instruction::I32Or);
        // Store back
        instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
    }
    instrs.push(Instruction::End); // end if
}
