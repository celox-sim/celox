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
    backend::{
        MemoryLayout,
        memory_layout::{
            RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
            RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
            RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
        },
    },
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
    if width == 0 { 1 } else { width.div_ceil(64) }
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
    let mem_pages = layout.merged_total_size.div_ceil(65536);
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
    for (&reg, ty) in &unit.register_map {
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
        SIRInstruction::Store(addr, offset, op_width, src, triggers, comb_capture_sites) => {
            compile_store(
                addr,
                offset,
                *op_width,
                src,
                triggers,
                comb_capture_sites,
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
        SIRInstruction::Slice(dst, src, bit_offset, width) => {
            compile_slice(dst, src, *bit_offset, *width, four_state, locals, instrs);
        }
        SIRInstruction::Mux(dst, cond, then_val, else_val) => {
            compile_mux(
                dst, cond, then_val, else_val, unit, four_state, locals, instrs,
            );
        }
        SIRInstruction::RuntimeEvent { site_id, args } => {
            compile_runtime_event(
                STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
                None,
                *site_id,
                args,
                None,
                layout,
                locals,
                instrs,
            );
        }
        SIRInstruction::CombCaptureEvent {
            site_id,
            args,
            fatal_error_code,
            consume_enabled,
        } => {
            compile_runtime_event(
                STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
                Some((*site_id, *consume_enabled)),
                *site_id,
                args,
                *fatal_error_code,
                layout,
                locals,
                instrs,
            );
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
            compile_comb_capture_enable_if_changed(old, new, sites, four_state, locals, instrs);
        }
    }
}

fn compile_runtime_event(
    event_header_offset: usize,
    enabled_site: Option<(u32, bool)>,
    site_id: u32,
    args: &[RegisterId],
    fatal_error_code: Option<i64>,
    layout: &MemoryLayout,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let enabled_ptr = enabled_site.map(|(site, _)| {
        let ptr = locals.alloc(1);
        instrs.push(Instruction::I32Const(
            STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
        ));
        instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        instrs.push(Instruction::LocalSet(ptr));
        instrs.push(Instruction::LocalGet(ptr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
            offset: site as u64,
            align: 0,
            memory_index: 0,
        }));
        instrs.push(Instruction::I32Const(0));
        instrs.push(Instruction::I32Ne);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        ptr
    });

    emit_runtime_event_write(event_header_offset, site_id, args, layout, locals, instrs);

    if let Some((site, consume_enabled)) = enabled_site {
        if consume_enabled {
            let ptr = enabled_ptr.expect("enabled pointer must be available");
            instrs.push(Instruction::LocalGet(ptr));
            instrs.push(Instruction::I32WrapI64);
            instrs.push(Instruction::I32Const(0));
            instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: site as u64,
                align: 0,
                memory_index: 0,
            }));
        }
    }

    if let Some(code) = fatal_error_code {
        instrs.push(Instruction::I64Const(code));
        instrs.push(Instruction::Return);
    }
    if enabled_site.is_some() {
        instrs.push(Instruction::End);
    }
}

fn emit_runtime_event_write(
    event_header_offset: usize,
    site_id: u32,
    args: &[RegisterId],
    layout: &MemoryLayout,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let event_ptr = locals.alloc(1);
    let seq = locals.alloc(1);
    let slot_addr = locals.alloc(1);

    instrs.push(Instruction::I32Const(event_header_offset as i32));
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    instrs.push(Instruction::LocalSet(event_ptr));

    instrs.push(Instruction::LocalGet(event_ptr));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    instrs.push(Instruction::LocalSet(seq));

    instrs.push(Instruction::LocalGet(event_ptr));
    instrs.push(Instruction::I64Const(RUNTIME_EVENT_HEADER_SIZE as i64));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(seq));
    instrs.push(Instruction::I64Const(
        (layout.runtime_event_capacity as i64) - 1,
    ));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Const(layout.runtime_event_slot_size as i64));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(slot_addr));

    emit_i64_store_at_ptr_local(
        slot_addr,
        RUNTIME_EVENT_SLOT_SEQ_OFFSET,
        RUNTIME_EVENT_WRITING,
        instrs,
    );
    emit_i64_store_at_ptr_local(
        slot_addr,
        RUNTIME_EVENT_SLOT_SITE_OFFSET,
        site_id as u64,
        instrs,
    );
    emit_i64_store_at_ptr_local(
        slot_addr,
        RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        args.len() as u64,
        instrs,
    );

    if let Some(site_layout) = layout.runtime_event_site_layouts.get(site_id as usize) {
        for (idx, arg) in args.iter().enumerate() {
            let Some(arg_layout) = site_layout.args.get(idx) else {
                continue;
            };
            let reg = &locals.reg_map[arg];
            for word_idx in 0..arg_layout.word_count {
                instrs.push(Instruction::LocalGet(slot_addr));
                instrs.push(Instruction::I32WrapI64);
                if word_idx < reg.num_chunks {
                    instrs.push(Instruction::LocalGet(reg.value_idx + word_idx as u32));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                    offset: (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.value_word_offset + word_idx) * 8)
                        as u64,
                    align: 0,
                    memory_index: 0,
                }));

                instrs.push(Instruction::LocalGet(slot_addr));
                instrs.push(Instruction::I32WrapI64);
                if let Some(mask_idx) = reg.mask_idx {
                    if word_idx < reg.num_chunks {
                        instrs.push(Instruction::LocalGet(mask_idx + word_idx as u32));
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
                    offset: (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.mask_word_offset + word_idx) * 8)
                        as u64,
                    align: 0,
                    memory_index: 0,
                }));
            }
        }
    }

    instrs.push(Instruction::LocalGet(slot_addr));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::LocalGet(seq));
    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
        offset: RUNTIME_EVENT_SLOT_SEQ_OFFSET as u64,
        align: 0,
        memory_index: 0,
    }));

    instrs.push(Instruction::LocalGet(event_ptr));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::LocalGet(seq));
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
}

fn emit_i64_store_at_ptr_local(
    ptr_local: u32,
    offset: usize,
    value: u64,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(ptr_local));
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::I64Const(value as i64));
    instrs.push(Instruction::I64Store(wasm_encoder::MemArg {
        offset: offset as u64,
        align: 0,
        memory_index: 0,
    }));
}

fn compile_comb_capture_enable_if_changed(
    old: &RegisterId,
    new: &RegisterId,
    sites: &[u32],
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if sites.is_empty() {
        return;
    }
    let changed = locals.alloc(1);
    emit_regs_changed(old, new, four_state, locals, instrs);
    instrs.push(Instruction::I64ExtendI32U);
    instrs.push(Instruction::LocalSet(changed));
    emit_enable_comb_capture_sites(changed, sites, locals, instrs);
}

fn emit_regs_changed(
    old: &RegisterId,
    new: &RegisterId,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let old_reg = &locals.reg_map[old];
    let new_reg = &locals.reg_map[new];
    let chunks = old_reg.num_chunks.max(new_reg.num_chunks);
    instrs.push(Instruction::I32Const(0));
    for c in 0..chunks {
        emit_wide_get_chunk(instrs, old_reg, c);
        emit_wide_get_chunk(instrs, new_reg, c);
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I32Or);
    }
    if four_state {
        for c in 0..chunks {
            emit_mask_get_chunk(instrs, old_reg, c);
            emit_mask_get_chunk(instrs, new_reg, c);
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I32Or);
        }
    }
}

fn emit_mask_get_chunk(instrs: &mut Vec<Instruction<'static>>, reg: &RegLocal, chunk: usize) {
    if let Some(mask_idx) = reg.mask_idx
        && chunk < reg.num_chunks
    {
        instrs.push(Instruction::LocalGet(mask_idx + chunk as u32));
    } else {
        instrs.push(Instruction::I64Const(0));
    }
}

fn emit_enable_comb_capture_sites(
    changed_local: u32,
    sites: &[u32],
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let enabled_ptr = locals.alloc(1);
    instrs.push(Instruction::I32Const(
        STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
    ));
    instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
    instrs.push(Instruction::LocalSet(enabled_ptr));

    instrs.push(Instruction::LocalGet(changed_local));
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    for &site in sites {
        instrs.push(Instruction::LocalGet(enabled_ptr));
        instrs.push(Instruction::I32WrapI64);
        instrs.push(Instruction::I32Const(1));
        instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
            offset: site as u64,
            align: 0,
            memory_index: 0,
        }));
    }
    instrs.push(Instruction::End);
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

fn emit_slice_chunks(
    dst: &RegLocal,
    src: &RegLocal,
    bit_offset: usize,
    width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let num_dst_chunks = width.div_ceil(64);
    let mut remaining = width;
    let mut pos = bit_offset;

    for out_idx in 0..dst.num_chunks {
        if out_idx >= num_dst_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst.value_idx + out_idx as u32));
            continue;
        }

        let chunk_idx = pos / 64;
        let chunk_off = pos % 64;
        let chunk_width = remaining.min(64);
        let bits_in_chunk = (64 - chunk_off).min(remaining);

        emit_wide_get_chunk(instrs, src, chunk_idx);
        if chunk_off > 0 {
            instrs.push(Instruction::I64Const(chunk_off as i64));
            instrs.push(Instruction::I64ShrU);
        }

        if bits_in_chunk < remaining {
            emit_wide_get_chunk(instrs, src, chunk_idx + 1);
            instrs.push(Instruction::I64Const(bits_in_chunk as i64));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }

        emit_mask_to_width(instrs, chunk_width);
        instrs.push(Instruction::LocalSet(dst.value_idx + out_idx as u32));

        remaining -= chunk_width;
        pos += chunk_width;
    }
}

fn compile_slice(
    dst: &RegisterId,
    src: &RegisterId,
    bit_offset: usize,
    width: usize,
    four_state: bool,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = &locals.reg_map[dst];
    let s = locals.reg_map[src].clone();
    emit_slice_chunks(d, &s, bit_offset, width, instrs);

    if four_state {
        if let (Some(dst_mask), Some(src_mask)) = (d.mask_idx, s.mask_idx) {
            let dst_mask_reg = RegLocal {
                value_idx: dst_mask,
                num_chunks: d.num_chunks,
                mask_idx: None,
            };
            let src_mask_reg = RegLocal {
                value_idx: src_mask,
                num_chunks: s.num_chunks,
                mask_idx: None,
            };
            emit_slice_chunks(&dst_mask_reg, &src_mask_reg, bit_offset, width, instrs);
        }
    }
}

fn compile_mux(
    dst: &RegisterId,
    cond: &RegisterId,
    then_val: &RegisterId,
    else_val: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d_width = unit.register_map[dst].width();
    let d_chunks = locals.reg_map[dst].num_chunks;
    let dst_reg = locals.reg_map[dst].clone();
    let cond_reg = locals.reg_map[cond].clone();
    let then_reg = locals.reg_map[then_val].clone();
    let else_reg = locals.reg_map[else_val].clone();
    let cond_local = cond_reg.value_idx;

    // Allocate temps for cond_bc and not_cond_bc
    let tmp_cbc = locals.alloc(1);
    let tmp_ncbc = locals.alloc(1);

    // cond_bc = 0 - cond (all ones if cond=1, all zeros if cond=0)
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalGet(cond_local));
    instrs.push(Instruction::I64Sub);
    instrs.push(Instruction::LocalTee(tmp_cbc));

    // not_cond_bc = ~cond_bc
    instrs.push(Instruction::I64Const(-1));
    instrs.push(Instruction::I64Xor);
    instrs.push(Instruction::LocalSet(tmp_ncbc));

    for i in 0..d_chunks {
        let tv_local = then_reg.value_idx + i as u32;
        let ev_local = else_reg.value_idx + i as u32;
        let dst_local = dst_reg.value_idx + i as u32;

        // masked_then = cond_bc & then_val
        instrs.push(Instruction::LocalGet(tmp_cbc));
        instrs.push(Instruction::LocalGet(tv_local));
        instrs.push(Instruction::I64And);

        // masked_else = ~cond_bc & else_val
        instrs.push(Instruction::LocalGet(tmp_ncbc));
        instrs.push(Instruction::LocalGet(ev_local));
        instrs.push(Instruction::I64And);

        // result = masked_then | masked_else
        instrs.push(Instruction::I64Or);

        // Width mask for last chunk
        if i == d_chunks - 1 {
            let last_bits = d_width % 64;
            if last_bits != 0 {
                let mask = (1u64 << last_bits) - 1;
                instrs.push(Instruction::I64Const(mask as i64));
                instrs.push(Instruction::I64And);
            }
        }

        instrs.push(Instruction::LocalSet(dst_local));
    }

    if four_state {
        let (Some(dst_mask), Some(cond_mask), Some(then_mask), Some(else_mask)) = (
            dst_reg.mask_idx,
            cond_reg.mask_idx,
            then_reg.mask_idx,
            else_reg.mask_idx,
        ) else {
            return;
        };

        let cond_has_x = locals.alloc(1);
        instrs.push(Instruction::LocalGet(cond_mask));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(cond_has_x));

        for i in 0..d_chunks {
            let chunk_mask = chunk_mask_for_width(i, d_chunks, d_width);
            instrs.push(Instruction::LocalGet(cond_has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));

            instrs.push(Instruction::LocalGet(tmp_cbc));
            instrs.push(Instruction::LocalGet(then_mask + i as u32));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(tmp_ncbc));
            instrs.push(Instruction::LocalGet(else_mask + i as u32));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            emit_chunk_mask_to_width(instrs, i, d_chunks, d_width);
            instrs.push(Instruction::LocalSet(dst_mask + i as u32));

            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(chunk_mask as i64));
            instrs.push(Instruction::LocalSet(dst_mask + i as u32));
            instrs.push(Instruction::End);
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
    let l_width = unit.register_map[lhs].width();
    let r_width = unit.register_map[rhs].width();

    if d_num == 1 && l_num == 1 && r_num == 1 {
        compile_binary_narrow(
            dst, lhs, op, rhs, d_width, l_width, r_width, four_state, locals, instrs,
        );
    } else {
        compile_binary_wide(dst, lhs, op, rhs, d_width, unit, four_state, locals, instrs);
    }
}

fn compile_binary_narrow(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    d_width: usize,
    l_width: usize,
    r_width: usize,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let l = locals.reg_map[lhs].clone();
    let r = locals.reg_map[rhs].clone();

    if four_state && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        let compare_mask = locals.alloc(1);
        instrs.push(Instruction::LocalGet(
            r.mask_idx.expect("four-state rhs mask"),
        ));
        instrs.push(Instruction::I64Const(-1));
        instrs.push(Instruction::I64Xor);
        emit_mask_to_width(instrs, l_width.max(r_width));
        instrs.push(Instruction::LocalSet(compare_mask));

        instrs.push(Instruction::LocalGet(l.value_idx));
        instrs.push(Instruction::LocalGet(compare_mask));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalGet(r.value_idx));
        instrs.push(Instruction::LocalGet(compare_mask));
        instrs.push(Instruction::I64And);
        if matches!(op, BinaryOp::EqWildcard) {
            instrs.push(Instruction::I64Eq);
        } else {
            instrs.push(Instruction::I64Ne);
        }
        instrs.push(Instruction::I64ExtendI32U);
        emit_mask_to_width(instrs, d_width);
        instrs.push(Instruction::LocalSet(d.value_idx));

        compile_binary_mask_narrow(&d, &l, op, &r, d_width, locals, instrs);
        normalize_reg_with_mask(&d, d_width, instrs);
        return;
    }

    // Load operands
    instrs.push(Instruction::LocalGet(l.value_idx));
    instrs.push(Instruction::LocalGet(r.value_idx));

    match op {
        BinaryOp::Add => instrs.push(Instruction::I64Add),
        BinaryOp::Sub => instrs.push(Instruction::I64Sub),
        BinaryOp::Mul => instrs.push(Instruction::I64Mul),
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
            let quotient = matches!(op, BinaryOp::DivU | BinaryOp::DivS);
            let tmp_l = locals.alloc(1);
            let tmp_r = locals.alloc(1);
            instrs.push(Instruction::LocalSet(tmp_r));
            instrs.push(Instruction::LocalSet(tmp_l));
            if signed {
                emit_sign_extend(instrs, tmp_l, l_width.max(1));
                instrs.push(Instruction::LocalSet(tmp_l));
                emit_sign_extend(instrs, tmp_r, r_width.max(1));
                instrs.push(Instruction::LocalSet(tmp_r));
            }
            instrs.push(Instruction::LocalGet(tmp_r));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                ValType::I64,
            )));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::Else);
            if signed {
                // WebAssembly signed division traps on MIN / -1. SIR arithmetic
                // wraps that quotient to MIN and defines the remainder as zero.
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::I64Const(i64::MIN));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Eq);
                instrs.push(Instruction::I32And);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I64,
                )));
                if quotient {
                    instrs.push(Instruction::LocalGet(tmp_l));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                instrs.push(Instruction::Else);
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(if quotient {
                    Instruction::I64DivS
                } else {
                    Instruction::I64RemS
                });
                instrs.push(Instruction::End);
            } else {
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(if quotient {
                    Instruction::I64DivU
                } else {
                    Instruction::I64RemU
                });
            }
            instrs.push(Instruction::End);
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
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
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
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
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
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
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
            emit_sign_extend(instrs, l.value_idx, l_width.max(1));
            emit_sign_extend(instrs, r.value_idx, r_width.max(1));
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

    if four_state {
        compile_binary_mask_narrow(&d, &l, op, &r, d_width, locals, instrs);
        normalize_reg_with_mask(&d, d_width, instrs);
    }
}

fn compile_binary_wide(
    dst: &RegisterId,
    lhs: &RegisterId,
    op: &BinaryOp,
    rhs: &RegisterId,
    d_width: usize,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let l = locals.reg_map[lhs].clone();
    let r = locals.reg_map[rhs].clone();
    let d_chunks = d.num_chunks;
    let l_width = unit.register_map[lhs].width();
    let r_width = unit.register_map[rhs].width();

    if four_state && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        let compare_mask = RegLocal {
            value_idx: locals.alloc(d_chunks),
            num_chunks: d_chunks,
            mask_idx: None,
        };
        for c in 0..d_chunks {
            if c < r.num_chunks {
                instrs.push(Instruction::LocalGet(
                    r.mask_idx.expect("four-state rhs mask") + c as u32,
                ));
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                emit_chunk_mask_to_width(instrs, c, d_chunks, d_width);
                instrs.push(Instruction::LocalSet(compare_mask.value_idx + c as u32));
            } else {
                let chunk_mask = chunk_mask_for_width(c, d_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(compare_mask.value_idx + c as u32));
            }
        }

        instrs.push(Instruction::I64Const(1));
        for c in 0..d_chunks {
            emit_wide_get_chunk(instrs, &l, c);
            emit_wide_get_chunk(instrs, &compare_mask, c);
            instrs.push(Instruction::I64And);
            emit_wide_get_chunk(instrs, &r, c);
            emit_wide_get_chunk(instrs, &compare_mask, c);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64And);
        }
        if matches!(op, BinaryOp::NeWildcard) {
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::I64Xor);
        }
        instrs.push(Instruction::LocalSet(d.value_idx));
        for c in 1..d_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
        }

        compile_binary_mask_wide(&d, &l, op, &r, d_width, locals, instrs);
        normalize_reg_with_mask(&d, d_width, instrs);
        return;
    }

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
            let d1 = locals.alloc(1);
            let c1 = locals.alloc(1);
            let c2 = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(borrow));
            for c in 0..d_chunks {
                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64Sub);
                instrs.push(Instruction::LocalSet(d1));

                emit_wide_get_chunk(instrs, &l, c);
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::I64LtU);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(c1));
                instrs.push(Instruction::LocalGet(d1));
                instrs.push(Instruction::LocalGet(borrow));
                instrs.push(Instruction::I64Sub);
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
                instrs.push(Instruction::LocalGet(d1));
                instrs.push(Instruction::LocalGet(borrow));
                instrs.push(Instruction::I64LtU);
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(c2));
                instrs.push(Instruction::LocalGet(c1));
                instrs.push(Instruction::LocalGet(c2));
                instrs.push(Instruction::I64Or);
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
            let result = locals.alloc(1);
            let decided = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(result));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(decided));
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
                let tmp_l = locals.alloc(1);
                let tmp_r = locals.alloc(1);
                instrs.push(Instruction::LocalSet(tmp_r));
                instrs.push(Instruction::LocalSet(tmp_l));

                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                instrs.push(Instruction::I64Ne);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::LocalGet(tmp_l));
                instrs.push(Instruction::LocalGet(tmp_r));
                match op {
                    BinaryOp::LtU | BinaryOp::LeU => instrs.push(Instruction::I64LtU),
                    BinaryOp::GtU | BinaryOp::GeU => instrs.push(Instruction::I64GtU),
                    _ => unreachable!(),
                }
                instrs.push(Instruction::I64ExtendI32U);
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(decided));
                instrs.push(Instruction::End);
                instrs.push(Instruction::End);
            }
            if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::End);
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
            // Signs same: compare from MSB to LSB and stop at the first differing chunk.
            let decided = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(result));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(decided));
            for c in (0..l.num_chunks.max(r.num_chunks)).rev() {
                let tmp_l = locals.alloc(1);
                let tmp_r = locals.alloc(1);
                emit_wide_get_chunk(instrs, &l, c);
                instrs.push(Instruction::LocalSet(tmp_l));
                emit_wide_get_chunk(instrs, &r, c);
                instrs.push(Instruction::LocalSet(tmp_r));
                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
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
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(decided));
                instrs.push(Instruction::End);
                instrs.push(Instruction::End);
            }
            if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
                instrs.push(Instruction::LocalGet(decided));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(result));
                instrs.push(Instruction::End);
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
        BinaryOp::Mul => {
            let carry = locals.alloc(1);
            let tmp_lo = locals.alloc(1);
            let tmp_rhs = locals.alloc(1);
            let tmp_hi = locals.alloc(1);
            let sum1 = locals.alloc(1);
            let c1 = locals.alloc(1);
            let c2 = locals.alloc(1);

            for c in 0..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(d.value_idx + c as u32));
            }

            for i in 0..d_chunks {
                emit_wide_get_chunk(instrs, &l, i);
                instrs.push(Instruction::LocalSet(tmp_lo));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(carry));

                for j in 0..d_chunks {
                    let k = i + j;
                    if k >= d_chunks {
                        break;
                    }

                    emit_wide_get_chunk(instrs, &r, j);
                    instrs.push(Instruction::LocalSet(tmp_rhs));
                    instrs.push(Instruction::LocalGet(tmp_rhs));
                    instrs.push(Instruction::LocalGet(tmp_lo));
                    instrs.push(Instruction::I64Mul);
                    instrs.push(Instruction::LocalSet(sum1));

                    emit_u64_mul_hi(instrs, tmp_lo, tmp_rhs, tmp_hi, locals);

                    instrs.push(Instruction::LocalGet(d.value_idx + k as u32));
                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalSet(sum1));

                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::LocalGet(d.value_idx + k as u32));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c1));

                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::LocalGet(carry));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalTee(d.value_idx + k as u32));
                    instrs.push(Instruction::LocalGet(sum1));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c2));

                    instrs.push(Instruction::LocalGet(tmp_hi));
                    instrs.push(Instruction::LocalGet(c1));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalGet(c2));
                    instrs.push(Instruction::I64Add);
                    instrs.push(Instruction::LocalSet(carry));
                }
            }
        }
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
            let quotient = matches!(op, BinaryOp::DivU | BinaryOp::DivS);
            let (dividend, lhs_negative) = if signed {
                prepare_wide_signed_magnitude(&l, l_width, d_chunks, locals, instrs)
            } else {
                (l.clone(), None)
            };
            let (divisor, rhs_negative) = if signed {
                prepare_wide_signed_magnitude(&r, r_width, d_chunks, locals, instrs)
            } else {
                (r.clone(), None)
            };
            let divisor_is_zero = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            for c in 0..d_chunks {
                emit_wide_get_chunk(instrs, &divisor, c);
                instrs.push(Instruction::I64Or);
            }
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::LocalSet(divisor_is_zero));

            let q = locals.alloc(d_chunks);
            let rem = locals.alloc(d_chunks);
            let cand = locals.alloc(d_chunks);
            let ge = locals.alloc(1);
            let borrow = locals.alloc(1);
            let d1 = locals.alloc(1);
            let c1 = locals.alloc(1);
            let c2 = locals.alloc(1);

            for c in 0..d_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(q + c as u32));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(rem + c as u32));
            }

            for bit in (0..d_width).rev() {
                let chunk_idx = bit / 64;
                let bit_idx = bit % 64;

                for c in (0..d_chunks).rev() {
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    instrs.push(Instruction::I64Const(1));
                    instrs.push(Instruction::I64Shl);
                    if c > 0 {
                        instrs.push(Instruction::LocalGet(rem + (c as u32 - 1)));
                        instrs.push(Instruction::I64Const(63));
                        instrs.push(Instruction::I64ShrU);
                        instrs.push(Instruction::I64Or);
                    }
                    instrs.push(Instruction::LocalSet(rem + c as u32));
                }

                instrs.push(Instruction::LocalGet(rem));
                emit_wide_get_chunk(instrs, &dividend, chunk_idx);
                instrs.push(Instruction::I64Const(bit_idx as i64));
                instrs.push(Instruction::I64ShrU);
                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(rem));

                instrs.push(Instruction::I64Const(1));
                instrs.push(Instruction::LocalSet(ge));
                for c in 0..d_chunks {
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64Ne);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64GtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(ge));
                    instrs.push(Instruction::End);
                }

                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(borrow));
                for c in 0..d_chunks {
                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64Sub);
                    instrs.push(Instruction::LocalSet(d1));

                    instrs.push(Instruction::LocalGet(rem + c as u32));
                    emit_wide_get_chunk(instrs, &divisor, c);
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c1));

                    instrs.push(Instruction::LocalGet(d1));
                    instrs.push(Instruction::LocalGet(borrow));
                    instrs.push(Instruction::I64Sub);
                    instrs.push(Instruction::LocalSet(cand + c as u32));

                    instrs.push(Instruction::LocalGet(d1));
                    instrs.push(Instruction::LocalGet(borrow));
                    instrs.push(Instruction::I64LtU);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::LocalSet(c2));

                    instrs.push(Instruction::LocalGet(c1));
                    instrs.push(Instruction::LocalGet(c2));
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(borrow));
                }

                instrs.push(Instruction::LocalGet(ge));
                instrs.push(Instruction::I32WrapI64);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                for c in 0..d_chunks {
                    instrs.push(Instruction::LocalGet(cand + c as u32));
                    instrs.push(Instruction::LocalSet(rem + c as u32));
                }
                instrs.push(Instruction::LocalGet(q + chunk_idx as u32));
                instrs.push(Instruction::I64Const((1u64 << bit_idx) as i64));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(q + chunk_idx as u32));
                instrs.push(Instruction::End);
            }

            let magnitude = RegLocal {
                value_idx: if quotient { q } else { rem },
                num_chunks: d_chunks,
                mask_idx: None,
            };
            let signed_result = if signed {
                let result_negative = if quotient {
                    let result = locals.alloc(1);
                    instrs.push(Instruction::LocalGet(lhs_negative.unwrap()));
                    instrs.push(Instruction::LocalGet(rhs_negative.unwrap()));
                    instrs.push(Instruction::I64Xor);
                    instrs.push(Instruction::LocalSet(result));
                    result
                } else {
                    lhs_negative.unwrap()
                };
                conditional_negate_wide_local(&magnitude, result_negative, d_chunks, locals, instrs)
            } else {
                magnitude
            };
            for c in 0..d_chunks {
                instrs.push(Instruction::LocalGet(divisor_is_zero));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I64,
                )));
                emit_wide_get_chunk(instrs, &signed_result, c);
                instrs.push(Instruction::Else);
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::End);
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

    if four_state {
        compile_binary_mask_wide(&d, &l, op, &r, d_width, locals, instrs);
        normalize_reg_with_mask(&d, d_width, instrs);
    }
}

fn compile_binary_mask_narrow(
    dst: &RegLocal,
    lhs: &RegLocal,
    op: &BinaryOp,
    rhs: &RegLocal,
    d_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (Some(dst_mask), Some(lhs_mask), Some(rhs_mask)) =
        (dst.mask_idx, lhs.mask_idx, rhs.mask_idx)
    else {
        return;
    };

    match op {
        BinaryOp::And => {
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
        }
        BinaryOp::Or => {
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::I64Or);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
        }
        BinaryOp::Xor => {
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
        }
        BinaryOp::LogicAnd => {
            let l_vm = locals.alloc(1);
            let r_vm = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(l_vm));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(r_vm));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(any_x));

            instrs.push(Instruction::LocalGet(l_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(r_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
        }
        BinaryOp::LogicOr => {
            let l_def_true = locals.alloc(1);
            let r_def_true = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(l_def_true));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(r_def_true));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(any_x));

            instrs.push(Instruction::LocalGet(l_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(r_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            let rhs_has_x = locals.alloc(1);
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::LocalSet(rhs_has_x));

            instrs.push(Instruction::LocalGet(rhs_has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            match op {
                BinaryOp::Shl => instrs.push(Instruction::I64Shl),
                BinaryOp::Shr => instrs.push(Instruction::I64ShrU),
                BinaryOp::Sar => instrs.push(Instruction::I64ShrS),
                _ => unreachable!(),
            }
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let compare_mask = locals.alloc(1);
            let lhs_unknown = locals.alloc(1);
            let mismatch = locals.alloc(1);

            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            emit_mask_to_width(instrs, d_width);
            instrs.push(Instruction::LocalSet(compare_mask));

            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(compare_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(lhs_unknown));

            instrs.push(Instruction::LocalGet(lhs.value_idx));
            instrs.push(Instruction::LocalGet(rhs.value_idx));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::LocalGet(compare_mask));
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::I64Xor);
            instrs.push(Instruction::I64And);
            instrs.push(Instruction::LocalSet(mismatch));

            instrs.push(Instruction::LocalGet(mismatch));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(lhs_unknown));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
        _ => {
            let any_x = locals.alloc(1);
            instrs.push(Instruction::LocalGet(lhs_mask));
            instrs.push(Instruction::LocalGet(rhs_mask));
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(any_x));
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask));
            instrs.push(Instruction::End);
        }
    }
}

fn compile_binary_mask_wide(
    dst: &RegLocal,
    lhs: &RegLocal,
    op: &BinaryOp,
    rhs: &RegLocal,
    d_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (Some(dst_mask_idx), Some(lhs_mask_idx), Some(rhs_mask_idx)) =
        (dst.mask_idx, lhs.mask_idx, rhs.mask_idx)
    else {
        return;
    };

    let dst_mask = RegLocal {
        value_idx: dst_mask_idx,
        num_chunks: dst.num_chunks,
        mask_idx: None,
    };
    let lhs_mask = RegLocal {
        value_idx: lhs_mask_idx,
        num_chunks: lhs.num_chunks,
        mask_idx: None,
    };
    let rhs_mask = RegLocal {
        value_idx: rhs_mask_idx,
        num_chunks: rhs.num_chunks,
        mask_idx: None,
    };

    match op {
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            for c in 0..dst.num_chunks {
                match op {
                    BinaryOp::And => {
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        instrs.push(Instruction::I64And);
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, rhs, c);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        emit_wide_get_chunk(instrs, lhs, c);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                    }
                    BinaryOp::Or => {
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        instrs.push(Instruction::I64And);
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, rhs, c);
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        emit_wide_get_chunk(instrs, lhs, c);
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                    }
                    BinaryOp::Xor => {
                        emit_wide_get_chunk(instrs, &lhs_mask, c);
                        emit_wide_get_chunk(instrs, &rhs_mask, c);
                        instrs.push(Instruction::I64Or);
                    }
                    _ => unreachable!(),
                }
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        BinaryOp::LogicAnd => {
            let l_vm = locals.alloc(1);
            let r_vm = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(l_vm));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(r_vm));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_x));

            for c in 0..lhs.num_chunks.max(rhs.num_chunks) {
                emit_wide_get_chunk(instrs, lhs, c);
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(l_vm));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(l_vm));

                emit_wide_get_chunk(instrs, rhs, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(r_vm));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(r_vm));

                emit_wide_get_chunk(instrs, &lhs_mask, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }

            instrs.push(Instruction::LocalGet(l_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(r_vm));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        BinaryOp::LogicOr => {
            let l_def_true = locals.alloc(1);
            let r_def_true = locals.alloc(1);
            let any_x = locals.alloc(1);

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(l_def_true));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(r_def_true));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_x));

            for c in 0..lhs.num_chunks.max(rhs.num_chunks) {
                emit_wide_get_chunk(instrs, lhs, c);
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(l_def_true));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(l_def_true));

                emit_wide_get_chunk(instrs, rhs, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(r_def_true));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(r_def_true));

                emit_wide_get_chunk(instrs, &lhs_mask, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }

            instrs.push(Instruction::LocalGet(l_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(r_def_true));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(any_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(
                chunk_mask_for_width(0, 1, d_width) as i64
            ));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            let rhs_has_x = locals.alloc(1);
            let shifted_mask = RegLocal {
                value_idx: locals.alloc(dst.num_chunks),
                num_chunks: dst.num_chunks,
                mask_idx: None,
            };

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(rhs_has_x));
            for c in 0..rhs_mask.num_chunks {
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::LocalGet(rhs_has_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(rhs_has_x));
            }

            match op {
                BinaryOp::Shl | BinaryOp::Shr => {
                    emit_wide_shift(
                        op,
                        &lhs_mask,
                        rhs,
                        &shifted_mask,
                        dst.num_chunks,
                        locals,
                        instrs,
                    );
                }
                BinaryOp::Sar => {
                    emit_wide_sar(
                        &lhs_mask,
                        rhs,
                        &shifted_mask,
                        dst.num_chunks,
                        d_width,
                        locals,
                        instrs,
                    );
                }
                _ => unreachable!(),
            }

            instrs.push(Instruction::LocalGet(rhs_has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::LocalGet(shifted_mask.value_idx + c as u32));
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
            instrs.push(Instruction::Else);
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
            instrs.push(Instruction::End);
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let any_unknown = locals.alloc(1);
            let any_mismatch = locals.alloc(1);

            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_unknown));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_mismatch));

            for c in 0..dst.num_chunks.max(lhs.num_chunks).max(rhs.num_chunks) {
                let compare_mask = locals.alloc(1);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalSet(compare_mask));

                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::LocalGet(compare_mask));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(any_unknown));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_unknown));

                emit_wide_get_chunk(instrs, lhs, c);
                emit_wide_get_chunk(instrs, rhs, c);
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::LocalGet(compare_mask));
                instrs.push(Instruction::I64And);
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                instrs.push(Instruction::I64Const(-1));
                instrs.push(Instruction::I64Xor);
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(any_mismatch));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_mismatch));
            }

            instrs.push(Instruction::LocalGet(any_mismatch));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(any_unknown));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(dst_mask.value_idx));
            instrs.push(Instruction::End);
            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
            }
        }
        _ => {
            let any_x = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(any_x));
            for c in 0..lhs_mask.num_chunks.max(rhs_mask.num_chunks) {
                emit_wide_get_chunk(instrs, &lhs_mask, c);
                emit_wide_get_chunk(instrs, &rhs_mask, c);
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(any_x));
            }
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::LocalGet(any_x));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
                instrs.push(Instruction::Else);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask.value_idx + c as u32));
                instrs.push(Instruction::End);
            }
        }
    }
}

fn normalize_reg_with_mask(reg: &RegLocal, width: usize, instrs: &mut Vec<Instruction<'static>>) {
    let Some(mask_idx) = reg.mask_idx else {
        return;
    };

    for c in 0..reg.num_chunks {
        instrs.push(Instruction::LocalGet(reg.value_idx + c as u32));
        instrs.push(Instruction::LocalGet(mask_idx + c as u32));
        instrs.push(Instruction::I64Or);
        emit_chunk_mask_to_width(instrs, c, reg.num_chunks, width);
        instrs.push(Instruction::LocalSet(reg.value_idx + c as u32));
    }
}

fn emit_u64_mul_hi(
    instrs: &mut Vec<Instruction<'static>>,
    lhs_local: u32,
    rhs_local: u32,
    dst_local: u32,
    locals: &mut LocalAllocator,
) {
    let mask = locals.alloc(1);
    let a0 = locals.alloc(1);
    let a1 = locals.alloc(1);
    let b0 = locals.alloc(1);
    let b1 = locals.alloc(1);
    let lo_lo = locals.alloc(1);
    let lo_hi = locals.alloc(1);
    let hi_lo = locals.alloc(1);
    let hi_hi = locals.alloc(1);
    let cross = locals.alloc(1);

    instrs.push(Instruction::I64Const(0xFFFF_FFFFu64 as i64));
    instrs.push(Instruction::LocalSet(mask));

    instrs.push(Instruction::LocalGet(lhs_local));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(a0));
    instrs.push(Instruction::LocalGet(lhs_local));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(a1));

    instrs.push(Instruction::LocalGet(rhs_local));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(b0));
    instrs.push(Instruction::LocalGet(rhs_local));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalSet(b1));

    instrs.push(Instruction::LocalGet(a0));
    instrs.push(Instruction::LocalGet(b0));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(lo_lo));

    instrs.push(Instruction::LocalGet(a0));
    instrs.push(Instruction::LocalGet(b1));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(lo_hi));

    instrs.push(Instruction::LocalGet(a1));
    instrs.push(Instruction::LocalGet(b0));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(hi_lo));

    instrs.push(Instruction::LocalGet(a1));
    instrs.push(Instruction::LocalGet(b1));
    instrs.push(Instruction::I64Mul);
    instrs.push(Instruction::LocalSet(hi_hi));

    instrs.push(Instruction::LocalGet(lo_lo));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::LocalGet(lo_hi));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(hi_lo));
    instrs.push(Instruction::LocalGet(mask));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(cross));

    instrs.push(Instruction::LocalGet(hi_hi));
    instrs.push(Instruction::LocalGet(lo_hi));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(hi_lo));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalGet(cross));
    instrs.push(Instruction::I64Const(32));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(dst_local));
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
    let acc = locals.alloc(1);

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
    // Use a local accumulator to keep the WASM stack shape trivial.
    for i in 0..d_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(acc));
        for j in 0..l.num_chunks {
            let src_idx = j as i64;
            let target_idx = i as i64;

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
            instrs.push(Instruction::LocalGet(acc));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(bit_off));
                instrs.push(Instruction::I64ShrU);
            } else {
                instrs.push(Instruction::LocalGet(bit_off));
                instrs.push(Instruction::I64Shl);
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(acc));
            instrs.push(Instruction::End);

            let next_src = if matches!(op, BinaryOp::Shr) {
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
            instrs.push(Instruction::LocalGet(bit_off));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::I64Ne);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(acc));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            if matches!(op, BinaryOp::Shr) {
                instrs.push(Instruction::LocalGet(inv_bit));
                instrs.push(Instruction::I64Shl);
            } else {
                instrs.push(Instruction::LocalGet(inv_bit));
                instrs.push(Instruction::I64ShrU);
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(acc));
            instrs.push(Instruction::End);
            instrs.push(Instruction::End);
        }
        instrs.push(Instruction::LocalGet(acc));
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
    let word_off = locals.alloc(1);
    let bit_off = locals.alloc(1);
    let inv_bit = locals.alloc(1);
    let acc = locals.alloc(1);
    let cur_word = locals.alloc(1);
    let next_word = locals.alloc(1);
    let sign_fill = locals.alloc(1);

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

    let msb_chunk = ((d_width - 1) / 64) as u32;
    let msb_bit = ((d_width - 1) % 64) as i64;
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(sign_fill));
    instrs.push(Instruction::LocalGet(l.value_idx + msb_chunk));
    instrs.push(Instruction::I64Const(msb_bit));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I32WrapI64);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
    instrs.push(Instruction::I64Const(-1));
    instrs.push(Instruction::LocalSet(sign_fill));
    instrs.push(Instruction::End);

    for i in 0..d_chunks {
        instrs.push(Instruction::LocalGet(sign_fill));
        instrs.push(Instruction::LocalSet(cur_word));
        instrs.push(Instruction::LocalGet(sign_fill));
        instrs.push(Instruction::LocalSet(next_word));

        for j in 0..l.num_chunks {
            let src_idx = j as i64;

            instrs.push(Instruction::I64Const(i as i64));
            instrs.push(Instruction::LocalGet(word_off));
            instrs.push(Instruction::I64Add);
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            instrs.push(Instruction::LocalSet(cur_word));
            instrs.push(Instruction::End);

            instrs.push(Instruction::I64Const(i as i64 + 1));
            instrs.push(Instruction::LocalGet(word_off));
            instrs.push(Instruction::I64Add);
            instrs.push(Instruction::I64Const(src_idx));
            instrs.push(Instruction::I64Eq);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            instrs.push(Instruction::LocalGet(l.value_idx + j as u32));
            instrs.push(Instruction::LocalSet(next_word));
            instrs.push(Instruction::End);
        }

        instrs.push(Instruction::LocalGet(cur_word));
        instrs.push(Instruction::LocalGet(bit_off));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::LocalSet(acc));

        instrs.push(Instruction::LocalGet(bit_off));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::LocalGet(acc));
        instrs.push(Instruction::LocalGet(next_word));
        instrs.push(Instruction::LocalGet(inv_bit));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::LocalSet(acc));
        instrs.push(Instruction::End);

        instrs.push(Instruction::LocalGet(acc));
        instrs.push(Instruction::LocalSet(d.value_idx + i as u32));
    }
}

fn compile_unary(
    dst: &RegisterId,
    op: &UnaryOp,
    src: &RegisterId,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let d = locals.reg_map[dst].clone();
    let s = locals.reg_map[src].clone();
    let d_width = unit.register_map[dst].width();
    let s_width = unit.register_map[src].width();

    match op {
        UnaryOp::Ident => {
            for c in 0..d.num_chunks {
                emit_wide_get_chunk(instrs, &s, c);
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
                emit_wide_get_chunk(instrs, &s, c);
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
        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            compile_count_unary_value(&d, op, &s, d_width, s_width, locals, instrs);
        }
    }

    if four_state {
        compile_unary_mask(&d, op, &s, d_width, s_width, locals, instrs);
        if !matches!(op, UnaryOp::Ident | UnaryOp::BitNot) {
            normalize_reg_with_mask(&d, d_width, instrs);
        }
    }
}

fn compile_unary_mask(
    dst: &RegLocal,
    op: &UnaryOp,
    src: &RegLocal,
    d_width: usize,
    s_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (Some(dst_mask), Some(src_mask)) = (dst.mask_idx, src.mask_idx) else {
        return;
    };

    match op {
        UnaryOp::Ident | UnaryOp::BitNot => {
            for c in 0..dst.num_chunks {
                if c < src.num_chunks {
                    instrs.push(Instruction::LocalGet(src_mask + c as u32));
                } else {
                    instrs.push(Instruction::I64Const(0));
                }
                let width = s_width.min(d_width);
                emit_chunk_mask_to_width(instrs, c, dst.num_chunks, width);
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
        }
        UnaryOp::Minus | UnaryOp::LogicNot => {
            let has_x = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(has_x));
            for c in 0..src.num_chunks {
                instrs.push(Instruction::LocalGet(has_x));
                instrs.push(Instruction::LocalGet(src_mask + c as u32));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(has_x));
            }
            instrs.push(Instruction::LocalGet(has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::Else);
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::End);
        }
        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            let has_x = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(has_x));
            for c in 0..src.num_chunks {
                instrs.push(Instruction::LocalGet(has_x));
                instrs.push(Instruction::LocalGet(src_mask + c as u32));
                if c + 1 == src.num_chunks && !s_width.is_multiple_of(64) {
                    let valid_bits = s_width % 64;
                    instrs.push(Instruction::I64Const(((1u64 << valid_bits) - 1) as i64));
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(has_x));
            }
            instrs.push(Instruction::LocalGet(has_x));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
            for c in 0..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::Else);
            for c in 0..dst.num_chunks {
                let chunk_mask = chunk_mask_for_width(c, dst.num_chunks, d_width);
                instrs.push(Instruction::I64Const(chunk_mask as i64));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
            instrs.push(Instruction::End);
        }
        UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => {
            let has_x = locals.alloc(1);
            let definite = locals.alloc(1);
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(has_x));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(definite));

            for c in 0..src.num_chunks {
                let chunk_width = if c + 1 == src.num_chunks {
                    let top_bits = s_width % 64;
                    if top_bits == 0 { 64 } else { top_bits }
                } else {
                    64
                };
                let chunk_mask = chunk_mask_for_width(c, src.num_chunks, s_width);

                instrs.push(Instruction::LocalGet(has_x));
                instrs.push(Instruction::LocalGet(src_mask + c as u32));
                instrs.push(Instruction::I64Or);
                instrs.push(Instruction::LocalSet(has_x));

                match op {
                    UnaryOp::And => {
                        instrs.push(Instruction::LocalGet(definite));
                        instrs.push(Instruction::LocalGet(src.value_idx + c as u32));
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::LocalGet(src_mask + c as u32));
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Const(chunk_mask as i64));
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        instrs.push(Instruction::LocalSet(definite));
                    }
                    UnaryOp::Or => {
                        instrs.push(Instruction::LocalGet(definite));
                        instrs.push(Instruction::LocalGet(src.value_idx + c as u32));
                        instrs.push(Instruction::LocalGet(src_mask + c as u32));
                        instrs.push(Instruction::I64Const(-1));
                        instrs.push(Instruction::I64Xor);
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Const(chunk_mask as i64));
                        instrs.push(Instruction::I64And);
                        instrs.push(Instruction::I64Or);
                        instrs.push(Instruction::LocalSet(definite));
                    }
                    UnaryOp::Xor => {
                        let _ = chunk_width;
                    }
                    _ => unreachable!(),
                }
            }

            match op {
                UnaryOp::And | UnaryOp::Or => {
                    instrs.push(Instruction::LocalGet(definite));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::LocalGet(has_x));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::Else);
                    instrs.push(Instruction::I64Const(1));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::End);
                    instrs.push(Instruction::Else);
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::End);
                }
                UnaryOp::Xor => {
                    instrs.push(Instruction::LocalGet(has_x));
                    instrs.push(Instruction::I64Eqz);
                    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
                    instrs.push(Instruction::I64Const(0));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::Else);
                    instrs.push(Instruction::I64Const(1));
                    instrs.push(Instruction::LocalSet(dst_mask));
                    instrs.push(Instruction::End);
                }
                _ => unreachable!(),
            }

            for c in 1..dst.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(dst_mask + c as u32));
            }
        }
    }
}

fn emit_masked_source_chunk(
    src: &RegLocal,
    chunk: usize,
    source_width: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(src.value_idx + chunk as u32));
    if chunk + 1 == src.num_chunks && !source_width.is_multiple_of(64) {
        let valid_bits = source_width % 64;
        instrs.push(Instruction::I64Const(((1u64 << valid_bits) - 1) as i64));
        instrs.push(Instruction::I64And);
    }
}

fn compile_count_unary_value(
    dst: &RegLocal,
    op: &UnaryOp,
    src: &RegLocal,
    destination_width: usize,
    source_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    debug_assert!(source_width > 0);
    let count = locals.alloc(1);
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(count));

    match op {
        UnaryOp::PopCount => {
            for chunk in 0..src.num_chunks {
                instrs.push(Instruction::LocalGet(count));
                emit_masked_source_chunk(src, chunk, source_width, instrs);
                instrs.push(Instruction::I64Popcnt);
                instrs.push(Instruction::I64Add);
                instrs.push(Instruction::LocalSet(count));
            }
        }
        UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            let active = locals.alloc(1);
            let source_chunk = locals.alloc(1);
            instrs.push(Instruction::I64Const(1));
            instrs.push(Instruction::LocalSet(active));

            if matches!(op, UnaryOp::CountLeadingZeros) {
                for chunk in (0..src.num_chunks).rev() {
                    compile_zero_count_chunk(
                        src,
                        chunk,
                        source_width,
                        true,
                        count,
                        active,
                        source_chunk,
                        instrs,
                    );
                }
            } else {
                for chunk in 0..src.num_chunks {
                    compile_zero_count_chunk(
                        src,
                        chunk,
                        source_width,
                        false,
                        count,
                        active,
                        source_chunk,
                        instrs,
                    );
                }
            }
        }
        _ => unreachable!(),
    }

    instrs.push(Instruction::LocalGet(count));
    emit_mask_to_width(instrs, destination_width);
    instrs.push(Instruction::LocalSet(dst.value_idx));
    for chunk in 1..dst.num_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(dst.value_idx + chunk as u32));
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_zero_count_chunk(
    src: &RegLocal,
    chunk: usize,
    source_width: usize,
    leading: bool,
    count: u32,
    active: u32,
    source_chunk: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(active));
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::I64Ne);
    instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));

    emit_masked_source_chunk(src, chunk, source_width, instrs);
    instrs.push(Instruction::LocalSet(source_chunk));
    let valid_bits = if chunk + 1 == src.num_chunks && !source_width.is_multiple_of(64) {
        source_width % 64
    } else {
        64
    };

    if leading {
        instrs.push(Instruction::LocalGet(count));
        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Clz);
        if valid_bits < 64 {
            instrs.push(Instruction::I64Const((64 - valid_bits) as i64));
            instrs.push(Instruction::I64Sub);
        }
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(count));

        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(active));
    } else {
        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Empty));
        instrs.push(Instruction::LocalGet(count));
        instrs.push(Instruction::I64Const(valid_bits as i64));
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(count));
        instrs.push(Instruction::Else);
        instrs.push(Instruction::LocalGet(count));
        instrs.push(Instruction::LocalGet(source_chunk));
        instrs.push(Instruction::I64Ctz);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(count));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(active));
        instrs.push(Instruction::End);
    }
    instrs.push(Instruction::End);
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
    comb_capture_sites: &[u32],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    // width=0: identity Store optimized away by alias; emit triggers only.
    if op_width == 0 {
        if emit_triggers && !triggers.is_empty() {
            emit_trigger_detection(addr, triggers, layout, locals, instrs);
        }
        return;
    }
    let s = locals.reg_map[src].clone();
    let abs = addr.absolute_addr();
    let base_offset = compute_byte_offset(layout, &abs, addr.region);
    let var_width = layout.widths[&abs];
    let var_byte_size = get_byte_size(var_width);
    let old_value_probe = if comb_capture_sites.is_empty() {
        None
    } else {
        capture_store_old_value(offset, op_width, base_offset, locals, instrs)
    };
    let old_mask_local = if comb_capture_sites.is_empty()
        || !four_state
        || !layout.is_4states.get(&abs).copied().unwrap_or(false)
    {
        None
    } else {
        capture_store_old_value(
            offset,
            op_width,
            base_offset + var_byte_size,
            locals,
            instrs,
        )
    };

    match offset {
        SIROffset::Static(bit_off) => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            let store_offset = base_offset + byte_off;
            let effective_width = if *bit_off == 0
                && bit_shift == 0
                && op_width < var_width
                && s.num_chunks > num_i64_chunks(op_width)
            {
                var_width
            } else {
                op_width
            };

            compile_store_at_offset(&s, store_offset, bit_shift, effective_width, locals, instrs);

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
                            effective_width,
                            locals,
                            instrs,
                        );
                    } else {
                        // Source is 2-state, clear mask
                        let mask_store_offset = base_offset + var_byte_size + byte_off;
                        compile_store_zero(mask_store_offset, effective_width, instrs);
                    }
                }
            }
        }
        SIROffset::Dynamic(reg) => {
            let offset_reg_value_idx = locals.reg_map[reg].value_idx;
            compile_store_dynamic(
                &s,
                base_offset,
                offset_reg_value_idx,
                op_width,
                locals,
                instrs,
            );

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
                            offset_reg_value_idx,
                            op_width,
                            locals,
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

    // 4-state type boundary enforcement:
    // When storing to a 2-state variable, subsequent users of the same source
    // register must observe the mask-cleared value. This mirrors the native
    // backend and is required because combinational lowering can reuse the same
    // source register across expressions that conceptually flow through a bit variable.
    if four_state && !layout.is_4states.get(&abs).copied().unwrap_or(false) {
        if let Some(mask_idx) = s.mask_idx {
            for c in 0..s.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(mask_idx + c as u32));
            }
        }
    }

    if !comb_capture_sites.is_empty() {
        emit_store_comb_capture_enable(
            old_value_probe,
            old_mask_local,
            offset,
            op_width,
            base_offset,
            var_byte_size,
            four_state && layout.is_4states.get(&abs).copied().unwrap_or(false),
            comb_capture_sites,
            locals,
            instrs,
        );
    }

    // Trigger detection
    if emit_triggers && !triggers.is_empty() {
        emit_trigger_detection(addr, triggers, layout, locals, instrs);
    }
}

fn capture_store_old_value(
    offset: &SIROffset,
    op_width: usize,
    base_offset: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> Option<RegLocal> {
    if op_width == 0 {
        return None;
    }
    let num_chunks = num_i64_chunks(op_width);
    let value_idx = locals.alloc(num_chunks);
    let probe = RegLocal {
        value_idx,
        num_chunks,
        mask_idx: None,
    };
    emit_load_store_value(offset, op_width, base_offset, &probe, locals, instrs)?;
    Some(probe)
}

fn emit_load_store_value(
    offset: &SIROffset,
    op_width: usize,
    base_offset: usize,
    dst: &RegLocal,
    locals: &LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> Option<()> {
    match offset {
        SIROffset::Static(bit_off) => {
            let byte_off = bit_off / 8;
            let bit_shift = bit_off % 8;
            if bit_shift != 0 && op_width > 64 {
                return None;
            }
            if op_width <= 64 {
                let byte_len = get_byte_size(op_width + bit_shift);
                if byte_len > 8 {
                    return None;
                }
                emit_load_small_word(base_offset + byte_off, byte_len, instrs);
                if bit_shift != 0 {
                    instrs.push(Instruction::I64Const(bit_shift as i64));
                    instrs.push(Instruction::I64ShrU);
                }
                if op_width < 64 {
                    instrs.push(Instruction::I64Const(((1u64 << op_width) - 1) as i64));
                    instrs.push(Instruction::I64And);
                }
                instrs.push(Instruction::LocalSet(dst.value_idx));
            } else {
                let store_bytes = get_byte_size(op_width);
                for c in 0..dst.num_chunks {
                    let remaining = store_bytes.saturating_sub(c * 8);
                    let byte_len = remaining.min(8);
                    emit_load_small_word(base_offset + byte_off + c * 8, byte_len, instrs);
                    instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
                }
            }
        }
        SIROffset::Dynamic(reg) => {
            let dyn_bit_offset_local = locals.reg_map[reg].value_idx;
            if op_width <= 64 {
                compile_load_dynamic(dst, base_offset, dyn_bit_offset_local, op_width, instrs);
            } else {
                let store_bytes = get_byte_size(op_width);
                for c in 0..dst.num_chunks {
                    let remaining = store_bytes.saturating_sub(c * 8);
                    let byte_len = remaining.min(8);
                    instrs.push(Instruction::I64Const(base_offset as i64));
                    instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
                    instrs.push(Instruction::I64Const(3));
                    instrs.push(Instruction::I64ShrU);
                    instrs.push(Instruction::I64Add);
                    if c > 0 {
                        instrs.push(Instruction::I64Const((c * 8) as i64));
                        instrs.push(Instruction::I64Add);
                    }
                    let addr_local = dst.value_idx + c as u32;
                    instrs.push(Instruction::LocalSet(addr_local));
                    emit_load_small_word_dynamic(addr_local, byte_len, instrs);
                    instrs.push(Instruction::LocalSet(addr_local));
                }
            }
        }
    }
    Some(())
}

fn emit_store_comb_capture_enable(
    old_value_probe: Option<RegLocal>,
    old_mask_probe: Option<RegLocal>,
    offset: &SIROffset,
    op_width: usize,
    base_offset: usize,
    var_byte_size: usize,
    is_4state: bool,
    sites: &[u32],
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let Some(old_value_probe) = old_value_probe else {
        return;
    };
    let changed = locals.alloc(1);
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::LocalSet(changed));

    let new_value_probe = RegLocal {
        value_idx: locals.alloc(old_value_probe.num_chunks),
        num_chunks: old_value_probe.num_chunks,
        mask_idx: None,
    };
    if emit_load_store_value(
        offset,
        op_width,
        base_offset,
        &new_value_probe,
        locals,
        instrs,
    )
    .is_none()
    {
        return;
    }
    for c in 0..old_value_probe.num_chunks {
        instrs.push(Instruction::LocalGet(changed));
        instrs.push(Instruction::LocalGet(old_value_probe.value_idx + c as u32));
        instrs.push(Instruction::LocalGet(new_value_probe.value_idx + c as u32));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::LocalSet(changed));
    }

    if is_4state {
        if let Some(old_mask_probe) = old_mask_probe {
            let new_mask_probe = RegLocal {
                value_idx: locals.alloc(old_mask_probe.num_chunks),
                num_chunks: old_mask_probe.num_chunks,
                mask_idx: None,
            };
            if emit_load_store_value(
                offset,
                op_width,
                base_offset + var_byte_size,
                &new_mask_probe,
                locals,
                instrs,
            )
            .is_some()
            {
                for c in 0..old_mask_probe.num_chunks {
                    instrs.push(Instruction::LocalGet(changed));
                    instrs.push(Instruction::LocalGet(old_mask_probe.value_idx + c as u32));
                    instrs.push(Instruction::LocalGet(new_mask_probe.value_idx + c as u32));
                    instrs.push(Instruction::I64Ne);
                    instrs.push(Instruction::I64ExtendI32U);
                    instrs.push(Instruction::I64Or);
                    instrs.push(Instruction::LocalSet(changed));
                }
            }
        }
    }

    emit_enable_comb_capture_sites(changed, sites, locals, instrs);
}

fn compile_store_at_offset(
    src: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let store_bytes = get_byte_size(op_width);
    let num_chunks = num_i64_chunks(op_width);

    if num_chunks == 1 && op_width <= 64 && (bit_shift != 0 || !op_width.is_multiple_of(8)) {
        emit_partial_store_small(src, byte_offset, bit_shift, op_width, locals, instrs);
    } else if bit_shift == 0 {
        // Byte-aligned store. Break remaining bytes into power-of-2
        // sized stores (8/4/2/1) so every byte of the value is written.
        for c in 0..num_chunks {
            let remaining_bytes = store_bytes - c * 8;
            let src_local = (c < src.num_chunks).then_some(src.value_idx + c as u32);
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
                    if let Some(src_local) = src_local {
                        instrs.push(Instruction::LocalGet(src_local));
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
                } else {
                    if let Some(src_local) = src_local {
                        instrs.push(Instruction::LocalGet(src_local));
                        instrs.push(Instruction::I64Const((written * 8) as i64));
                        instrs.push(Instruction::I64ShrU);
                    } else {
                        instrs.push(Instruction::I64Const(0));
                    }
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
    } else {
        // Multi-chunk bit-offset store: complex.
        // TODO: implement multi-chunk bit-offset RMW store
    }
}

fn emit_partial_store_small(
    src: &RegLocal,
    byte_offset: usize,
    bit_shift: usize,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let affected_bytes = (bit_shift + op_width).div_ceil(8);
    let tmp = locals.alloc(1);

    emit_load_small_word(byte_offset, affected_bytes, instrs);
    let clear_mask = !((((1u128 << op_width) - 1) << bit_shift) as u64);
    instrs.push(Instruction::I64Const(mask_for_bytes(affected_bytes) as i64));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::I64Const(clear_mask as i64));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalGet(src.value_idx));
    if op_width < 64 {
        let src_mask = (1u64 << op_width) - 1;
        instrs.push(Instruction::I64Const(src_mask as i64));
        instrs.push(Instruction::I64And);
    }
    if bit_shift > 0 {
        instrs.push(Instruction::I64Const(bit_shift as i64));
        instrs.push(Instruction::I64Shl);
    }
    instrs.push(Instruction::I64Or);
    instrs.push(Instruction::LocalSet(tmp));
    emit_store_small_word(byte_offset, affected_bytes, tmp, instrs);
}

fn compile_store_zero(byte_offset: usize, op_width: usize, instrs: &mut Vec<Instruction<'static>>) {
    let store_bytes = get_byte_size(op_width);
    let num_chunks = store_bytes.div_ceil(8);
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
    locals: &mut LocalAllocator,
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

    if num_chunks == 1 && op_width <= 64 {
        let addr_local = locals.alloc(1);
        let bit_shift_local = locals.alloc(1);
        let tmp_local = locals.alloc(1);
        let affected_bytes = (7 + op_width).div_ceil(8);
        let op_mask = if op_width >= 64 {
            u64::MAX
        } else {
            (1u64 << op_width) - 1
        };

        instrs.push(Instruction::I64Const(base_offset as i64));
        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(3));
        instrs.push(Instruction::I64ShrU);
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalSet(addr_local));

        instrs.push(Instruction::LocalGet(dyn_bit_offset_local));
        instrs.push(Instruction::I64Const(7));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(bit_shift_local));

        emit_load_small_word_dynamic(addr_local, affected_bytes, instrs);
        instrs.push(Instruction::I64Const(mask_for_bytes(affected_bytes) as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::I64Const(op_mask as i64));
        instrs.push(Instruction::LocalGet(bit_shift_local));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Const(-1));
        instrs.push(Instruction::I64Xor);
        instrs.push(Instruction::I64And);

        instrs.push(Instruction::LocalGet(src.value_idx));
        if op_width < 64 {
            instrs.push(Instruction::I64Const(op_mask as i64));
            instrs.push(Instruction::I64And);
        }
        instrs.push(Instruction::LocalGet(bit_shift_local));
        instrs.push(Instruction::I64Shl);
        instrs.push(Instruction::I64Or);
        instrs.push(Instruction::LocalSet(tmp_local));

        emit_store_small_word_dynamic(addr_local, affected_bytes, tmp_local, instrs);
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

fn emit_dynamic_addr_with_offset(
    addr_local: u32,
    byte_offset: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    instrs.push(Instruction::LocalGet(addr_local));
    instrs.push(Instruction::I32WrapI64);
    if byte_offset > 0 {
        instrs.push(Instruction::I32Const(byte_offset as i32));
        instrs.push(Instruction::I32Add);
    }
}

fn emit_load_small_word_dynamic(
    addr_local: u32,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    emit_load_small_word_dynamic_at(addr_local, 0, byte_len, instrs);
}

fn emit_load_small_word_dynamic_at(
    addr_local: u32,
    start_off: usize,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match byte_len {
        0 => instrs.push(Instruction::I64Const(0)),
        1 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        2 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        3 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 2, instrs);
            emit_load_small_word_dynamic_at(addr_local, start_off + 2, 1, instrs);
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        4 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        5 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 4, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        6 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 4, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        7 => {
            emit_load_small_word_dynamic_at(addr_local, start_off, 6, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 6, instrs);
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
            instrs.push(Instruction::I64Const(48));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        _ => {
            emit_dynamic_addr_with_offset(addr_local, 0, instrs);
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
}

fn emit_store_small_word_dynamic(
    addr_local: u32,
    byte_len: usize,
    value_local: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    emit_store_small_word_dynamic_at(addr_local, 0, byte_len, value_local, instrs);
}

fn emit_store_small_word_dynamic_at(
    addr_local: u32,
    start_off: usize,
    byte_len: usize,
    value_local: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    match byte_len {
        0 => {}
        1 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store8(memarg));
        }
        2 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store16(memarg));
        }
        3 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 2, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 2, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store8(memarg));
        }
        4 => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store32(memarg));
        }
        5 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 4, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store8(memarg));
        }
        6 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 4, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 4, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store16(memarg));
        }
        7 => {
            emit_store_small_word_dynamic_at(addr_local, start_off, 6, value_local, instrs);
            emit_dynamic_addr_with_offset(addr_local, start_off + 6, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(48));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::I64Store8(memarg));
        }
        _ => {
            emit_dynamic_addr_with_offset(addr_local, start_off, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store(memarg));
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
                let mut copied = 0usize;
                while copied < copy_bytes {
                    let remaining = copy_bytes - copied;
                    let src_off = src_base + byte_off + copied;
                    let dst_off = dst_base + byte_off + copied;
                    instrs.push(Instruction::I32Const(dst_off as i32));
                    instrs.push(Instruction::I32Const(src_off as i32));
                    match remaining {
                        1 => {
                            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            copied += 1;
                        }
                        2 | 3 => {
                            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            instrs.push(Instruction::I32Store16(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            copied += 2;
                        }
                        4..=7 => {
                            instrs.push(Instruction::I32Load(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            instrs.push(Instruction::I32Store(wasm_encoder::MemArg {
                                offset: 0,
                                align: 0,
                                memory_index: 0,
                            }));
                            copied += 4;
                        }
                        _ => {
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
                            copied += 8;
                        }
                    }
                }

                // 4-state mask commit
                if four_state {
                    let is_4state = layout.is_4states.get(&dst_abs).copied().unwrap_or(false);
                    if is_4state {
                        let src_var_byte_size = get_byte_size(layout.widths[&src_abs]);
                        let dst_var_byte_size = get_byte_size(layout.widths[&dst_abs]);
                        let mut copied = 0usize;
                        while copied < copy_bytes {
                            let remaining = copy_bytes - copied;
                            let src_mask_off = src_base + src_var_byte_size + byte_off + copied;
                            let dst_mask_off = dst_base + dst_var_byte_size + byte_off + copied;
                            instrs.push(Instruction::I32Const(dst_mask_off as i32));
                            instrs.push(Instruction::I32Const(src_mask_off as i32));
                            match remaining {
                                1 => {
                                    instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                                        offset: 0,
                                        align: 0,
                                        memory_index: 0,
                                    }));
                                    instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
                                        offset: 0,
                                        align: 0,
                                        memory_index: 0,
                                    }));
                                    copied += 1;
                                }
                                2 | 3 => {
                                    instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                                        offset: 0,
                                        align: 0,
                                        memory_index: 0,
                                    }));
                                    instrs.push(Instruction::I32Store16(wasm_encoder::MemArg {
                                        offset: 0,
                                        align: 0,
                                        memory_index: 0,
                                    }));
                                    copied += 2;
                                }
                                4..=7 => {
                                    instrs.push(Instruction::I32Load(wasm_encoder::MemArg {
                                        offset: 0,
                                        align: 0,
                                        memory_index: 0,
                                    }));
                                    instrs.push(Instruction::I32Store(wasm_encoder::MemArg {
                                        offset: 0,
                                        align: 0,
                                        memory_index: 0,
                                    }));
                                    copied += 4;
                                }
                                _ => {
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
                                    copied += 8;
                                }
                            }
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

fn mask_for_bytes(bytes: usize) -> u64 {
    match bytes {
        0 => 0,
        8.. => u64::MAX,
        _ => (1u64 << (bytes * 8)) - 1,
    }
}

fn emit_load_small_word(
    byte_offset: usize,
    byte_len: usize,
    instrs: &mut Vec<Instruction<'static>>,
) {
    match byte_len {
        0 => instrs.push(Instruction::I64Const(0)),
        1 => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::I32Load8U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        2 => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::I32Load16U(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        3 => {
            emit_load_small_word(byte_offset, 2, instrs);
            emit_load_small_word(byte_offset + 2, 1, instrs);
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        4 => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::I32Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
            instrs.push(Instruction::I64ExtendI32U);
        }
        5 => {
            emit_load_small_word(byte_offset, 4, instrs);
            emit_load_small_word(byte_offset + 4, 1, instrs);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        6 => {
            emit_load_small_word(byte_offset, 4, instrs);
            emit_load_small_word(byte_offset + 4, 2, instrs);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        7 => {
            emit_load_small_word(byte_offset, 4, instrs);
            emit_load_small_word(byte_offset + 4, 2, instrs);
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
            emit_load_small_word(byte_offset + 6, 1, instrs);
            instrs.push(Instruction::I64Const(48));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        _ => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::I64Load(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
}

fn emit_store_small_word(
    byte_offset: usize,
    byte_len: usize,
    value_local: u32,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let memarg = wasm_encoder::MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    };
    match byte_len {
        0 => {}
        1 => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store8(memarg));
        }
        2 => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store16(memarg));
        }
        3 => {
            emit_store_small_word(byte_offset, 2, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word(byte_offset + 2, 1, value_local, instrs);
        }
        4 => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store32(memarg));
        }
        5 => {
            emit_store_small_word(byte_offset, 4, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word(byte_offset + 4, 1, value_local, instrs);
        }
        6 => {
            emit_store_small_word(byte_offset, 4, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word(byte_offset + 4, 2, value_local, instrs);
        }
        7 => {
            emit_store_small_word(byte_offset, 4, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(32));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word(byte_offset + 4, 2, value_local, instrs);
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Const(16));
            instrs.push(Instruction::I64ShrU);
            instrs.push(Instruction::LocalSet(value_local));
            emit_store_small_word(byte_offset + 6, 1, value_local, instrs);
        }
        _ => {
            instrs.push(Instruction::I32Const(byte_offset as i32));
            instrs.push(Instruction::LocalGet(value_local));
            instrs.push(Instruction::I64Store(memarg));
        }
    }
}

fn emit_mask_to_width(instrs: &mut Vec<Instruction<'static>>, width: usize) {
    if width > 0 && width < 64 {
        let mask = (1u64 << width) - 1;
        instrs.push(Instruction::I64Const(mask as i64));
        instrs.push(Instruction::I64And);
    }
}

fn chunk_mask_for_width(chunk: usize, num_chunks: usize, width: usize) -> u64 {
    if chunk + 1 < num_chunks {
        u64::MAX
    } else {
        let top_bits = width % 64;
        if top_bits == 0 {
            u64::MAX
        } else {
            (1u64 << top_bits) - 1
        }
    }
}

fn emit_chunk_mask_to_width(
    instrs: &mut Vec<Instruction<'static>>,
    chunk: usize,
    num_chunks: usize,
    width: usize,
) {
    let mask = chunk_mask_for_width(chunk, num_chunks, width);
    if mask != u64::MAX {
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

fn prepare_wide_signed_magnitude(
    source: &RegLocal,
    width: usize,
    num_chunks: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> (RegLocal, Option<u32>) {
    debug_assert!(width > 0);
    let sign = locals.alloc(1);
    let sign_bit = width - 1;
    emit_wide_get_chunk(instrs, source, sign_bit / 64);
    instrs.push(Instruction::I64Const((sign_bit % 64) as i64));
    instrs.push(Instruction::I64ShrU);
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::I64And);
    instrs.push(Instruction::LocalSet(sign));

    let extended = RegLocal {
        value_idx: locals.alloc(num_chunks),
        num_chunks,
        mask_idx: None,
    };
    let source_chunks = width.div_ceil(64);
    let top_bits = width % 64;
    for chunk in 0..num_chunks {
        if chunk >= source_chunks {
            instrs.push(Instruction::LocalGet(sign));
            instrs.push(Instruction::I64Eqz);
            instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                ValType::I64,
            )));
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::Else);
            instrs.push(Instruction::I64Const(-1));
            instrs.push(Instruction::End);
        } else {
            emit_wide_get_chunk(instrs, source, chunk);
            if chunk + 1 == source_chunks && top_bits != 0 {
                let low_mask = (1u64 << top_bits) - 1;
                instrs.push(Instruction::I64Const(low_mask as i64));
                instrs.push(Instruction::I64And);
                instrs.push(Instruction::LocalGet(sign));
                instrs.push(Instruction::I64Eqz);
                instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
                    ValType::I64,
                )));
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::Else);
                instrs.push(Instruction::I64Const((!low_mask) as i64));
                instrs.push(Instruction::End);
                instrs.push(Instruction::I64Or);
            }
        }
        instrs.push(Instruction::LocalSet(extended.value_idx + chunk as u32));
    }

    (
        conditional_negate_wide_local(&extended, sign, num_chunks, locals, instrs),
        Some(sign),
    )
}

fn conditional_negate_wide_local(
    source: &RegLocal,
    negate: u32,
    num_chunks: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> RegLocal {
    let negated = RegLocal {
        value_idx: locals.alloc(num_chunks),
        num_chunks,
        mask_idx: None,
    };
    let carry = locals.alloc(1);
    instrs.push(Instruction::I64Const(1));
    instrs.push(Instruction::LocalSet(carry));
    for chunk in 0..num_chunks {
        emit_wide_get_chunk(instrs, source, chunk);
        instrs.push(Instruction::I64Const(-1));
        instrs.push(Instruction::I64Xor);
        instrs.push(Instruction::LocalGet(carry));
        instrs.push(Instruction::I64Add);
        instrs.push(Instruction::LocalTee(negated.value_idx + chunk as u32));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::LocalGet(carry));
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::I64Ne);
        instrs.push(Instruction::I32And);
        instrs.push(Instruction::I64ExtendI32U);
        instrs.push(Instruction::LocalSet(carry));
    }

    let result = RegLocal {
        value_idx: locals.alloc(num_chunks),
        num_chunks,
        mask_idx: None,
    };
    for chunk in 0..num_chunks {
        instrs.push(Instruction::LocalGet(negate));
        instrs.push(Instruction::I64Eqz);
        instrs.push(Instruction::If(wasm_encoder::BlockType::Result(
            ValType::I64,
        )));
        emit_wide_get_chunk(instrs, source, chunk);
        instrs.push(Instruction::Else);
        emit_wide_get_chunk(instrs, &negated, chunk);
        instrs.push(Instruction::End);
        instrs.push(Instruction::LocalSet(result.value_idx + chunk as u32));
    }
    result
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
                    SIRInstruction::Store(addr, _, _, _, triggers, _) if !triggers.is_empty() => {
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

#[cfg(test)]
mod bit_count_tests {
    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;
    use wasmtime::{Engine, Linker, Memory, Module as WasmtimeModule, Store};

    use super::*;
    use crate::{
        SimulatorOptions,
        backend::JitEngine,
        ir::{BasicBlock, InstanceId},
    };

    const OUTPUT_OFFSET: usize = 32;
    const SLOT_BITS: usize = 128;

    #[derive(Clone)]
    struct CountCase {
        source_width: usize,
        destination_width: usize,
        value: BigUint,
        mask: BigUint,
        expected: [usize; 3],
    }

    fn address() -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        }
    }

    fn layout(variable_width: usize, four_state: bool) -> MemoryLayout {
        let addr = address();
        let mut offsets = HashMap::default();
        offsets.insert(addr, OUTPUT_OFFSET);
        let mut widths = HashMap::default();
        widths.insert(addr, variable_width);
        let mut is_4states = HashMap::default();
        is_4states.insert(addr, true);
        let variable_bytes = get_byte_size(variable_width);
        let total_size = OUTPUT_OFFSET + variable_bytes * usize::from(four_state) + variable_bytes;
        let working_base_offset = (total_size + 7) & !7;

        MemoryLayout {
            four_state,
            offsets,
            widths,
            is_4states,
            total_size,
            working_offsets: HashMap::default(),
            working_base_offset,
            merged_total_size: 65_536,
            triggered_bits_offset: working_base_offset,
            triggered_bits_total_size: 0,
            scratch_base_offset: working_base_offset,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: Vec::new(),
        }
    }

    fn execution_unit(cases: &[CountCase]) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        let mut instructions = Vec::new();
        let mut next_reg = 0usize;
        let ops = [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ];

        for (case_index, case) in cases.iter().enumerate() {
            let source = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(
                source,
                RegisterType::Logic {
                    width: case.source_width,
                },
            );
            instructions.push(SIRInstruction::Imm(
                source,
                SIRValue::new_four_state(case.value.clone(), case.mask.clone()),
            ));

            for (op_index, op) in ops.iter().copied().enumerate() {
                let destination = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(
                    destination,
                    RegisterType::Logic {
                        width: case.destination_width,
                    },
                );
                instructions.push(SIRInstruction::Unary(destination, op, source));
                let result_index = case_index * ops.len() + op_index;
                instructions.push(SIRInstruction::Store(
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                    SIROffset::Static(result_index * SLOT_BITS),
                    case.destination_width,
                    destination,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }

        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: std::iter::once((BlockId(0), block)).collect(),
            register_map,
        }
    }

    fn run_cranelift(
        unit: &ExecutionUnit<RegionedAbsoluteAddr>,
        layout: &MemoryLayout,
        four_state: bool,
    ) -> Vec<u8> {
        let options = SimulatorOptions {
            four_state,
            ..SimulatorOptions::default()
        };
        let mut engine = JitEngine::new(layout.clone(), &options).expect("create JIT engine");
        let code = engine
            .compile_units(std::slice::from_ref(unit), None, None, None)
            .expect("compile count operations");
        let function: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(code) };
        let mut memory = vec![0u8; layout.merged_total_size];
        let status = unsafe { function(memory.as_mut_ptr()) };
        assert_eq!(status, 0);
        memory
    }

    fn run_wasm(
        unit: &ExecutionUnit<RegionedAbsoluteAddr>,
        layout: &MemoryLayout,
        four_state: bool,
    ) -> Vec<u8> {
        let wasm = compile_units(std::slice::from_ref(unit), layout, four_state, false);
        let engine = Engine::default();
        let module = WasmtimeModule::new(&engine, &wasm.bytes).expect("compile Wasm module");
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, wasmtime::MemoryType::new(1, None))
            .expect("create Wasm memory");
        let mut linker = Linker::new(&engine);
        linker
            .define(&mut store, "env", "memory", memory)
            .expect("define Wasm memory");
        let instance = linker
            .instantiate(&mut store, &module)
            .expect("instantiate Wasm module");
        let run = instance
            .get_typed_func::<(), i64>(&mut store, "run")
            .expect("find Wasm entry point");
        assert_eq!(run.call(&mut store, ()).expect("execute Wasm"), 0);
        memory.data(&store).to_vec()
    }

    fn read_bits_at(memory: &[u8], base_offset: usize, bit_offset: usize, width: usize) -> BigUint {
        assert!(bit_offset.is_multiple_of(8));
        let byte_offset = base_offset + bit_offset / 8;
        BigUint::from_bytes_le(&memory[byte_offset..byte_offset + get_byte_size(width)])
            & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
    }

    fn read_bits(memory: &[u8], bit_offset: usize, width: usize) -> BigUint {
        read_bits_at(memory, OUTPUT_OFFSET, bit_offset, width)
    }

    fn assert_results(cases: &[CountCase], memory: &[u8]) {
        for (case_index, case) in cases.iter().enumerate() {
            for (op_index, expected) in case.expected.iter().copied().enumerate() {
                let result_index = case_index * 3 + op_index;
                assert_eq!(
                    read_bits(memory, result_index * SLOT_BITS, case.destination_width),
                    BigUint::from(expected),
                    "case {case_index}, operation {op_index}",
                );
            }
        }
    }

    #[test]
    fn bit_counts_match_for_narrow_and_wide_two_state_values() {
        let cases = vec![
            CountCase {
                source_width: 1,
                destination_width: 1,
                value: BigUint::from(0u8),
                mask: BigUint::from(0u8),
                expected: [0, 1, 1],
            },
            CountCase {
                source_width: 5,
                destination_width: 3,
                value: BigUint::from(0b0_0100u8),
                mask: BigUint::from(0u8),
                expected: [1, 2, 2],
            },
            CountCase {
                source_width: 63,
                destination_width: 6,
                value: (BigUint::from(1u8) << 62) | (BigUint::from(1u8) << 7),
                mask: BigUint::from(0u8),
                expected: [2, 0, 7],
            },
            CountCase {
                source_width: 64,
                destination_width: 7,
                value: BigUint::from(0u8),
                mask: BigUint::from(0u8),
                expected: [0, 64, 64],
            },
            CountCase {
                source_width: 65,
                destination_width: 7,
                value: (BigUint::from(1u8) << 64) | (BigUint::from(1u8) << 40),
                mask: BigUint::from(0u8),
                expected: [2, 0, 40],
            },
            CountCase {
                source_width: 70,
                destination_width: 80,
                value: BigUint::from(0u8),
                mask: BigUint::from(0u8),
                expected: [0, 70, 70],
            },
            CountCase {
                source_width: 130,
                destination_width: 8,
                value: (BigUint::from(1u8) << 128)
                    | (BigUint::from(1u8) << 65)
                    | (BigUint::from(1u8) << 3),
                mask: BigUint::from(0u8),
                expected: [3, 1, 3],
            },
        ];
        let variable_width = cases.len() * 3 * SLOT_BITS;
        let layout = layout(variable_width, false);
        let unit = execution_unit(&cases);

        assert_results(&cases, &run_cranelift(&unit, &layout, false));
        assert_results(&cases, &run_wasm(&unit, &layout, false));
    }

    #[test]
    fn bit_counts_propagate_unknown_as_full_destination_mask() {
        let cases = vec![
            CountCase {
                source_width: 5,
                destination_width: 3,
                value: BigUint::from(0b0_0100u8),
                mask: BigUint::from(0u8),
                expected: [1, 2, 2],
            },
            CountCase {
                source_width: 5,
                destination_width: 3,
                value: BigUint::from(0u8),
                mask: BigUint::from(1u8) << 2,
                expected: [0, 0, 0],
            },
            CountCase {
                source_width: 70,
                destination_width: 80,
                value: BigUint::from(0u8),
                mask: BigUint::from(1u8) << 66,
                expected: [0, 0, 0],
            },
        ];
        let variable_width = cases.len() * 3 * SLOT_BITS;
        let layout = layout(variable_width, true);
        let unit = execution_unit(&cases);
        let value_bytes = get_byte_size(variable_width);

        for memory in [
            run_cranelift(&unit, &layout, true),
            run_wasm(&unit, &layout, true),
        ] {
            for (case_index, case) in cases.iter().enumerate() {
                let expected_mask = if case.mask == BigUint::from(0u8) {
                    BigUint::from(0u8)
                } else {
                    (BigUint::from(1u8) << case.destination_width) - BigUint::from(1u8)
                };
                for (op_index, expected_count) in case.expected.iter().copied().enumerate() {
                    let result_index = case_index * 3 + op_index;
                    let bit_offset = result_index * SLOT_BITS;
                    let expected_value = if expected_mask == BigUint::from(0u8) {
                        BigUint::from(expected_count)
                    } else {
                        expected_mask.clone()
                    };
                    assert_eq!(
                        read_bits(&memory, bit_offset, case.destination_width),
                        expected_value,
                        "case {case_index}, operation {op_index} value",
                    );
                    assert_eq!(
                        read_bits_at(
                            &memory,
                            OUTPUT_OFFSET + value_bytes,
                            bit_offset,
                            case.destination_width,
                        ),
                        expected_mask,
                        "case {case_index}, operation {op_index} mask",
                    );
                }
            }
        }
    }
}
