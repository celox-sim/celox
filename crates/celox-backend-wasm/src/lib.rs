//! SIRT → WASM bytecode translator.
//!
//! Uses `wasm-encoder` to build a WASM module from SIRT [`ExecutionUnit`]s.
//! The generated module imports a linear memory ("env"/"memory") and exports
//! a single `"run"` function `() -> i64` (0 = success, non-zero = error).
//!
//! RegisterId → WASM local (i64 for ≤64-bit; multiple i64 locals for wide values).
//! Block control flow is implemented via `loop` + `block` + `br_table`.

pub use celox_sir::float::evaluate as evaluate_float;

use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction, MemoryType, Module, TypeSection, ValType,
};

pub type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub type HashSet<K> = fxhash::FxHashSet<K>;

use celox_design::{
    BinaryOp, FRAME_REGION, SPARSE_WORKING_REGION, STABLE_REGION, StateAddr, TriggerIdWithKind,
    UnaryOp,
};
use celox_sir::extern_abi::{ExternValue, SV_LOGIC_MASK_SHIFT};
use celox_sir::{
    BlockId, ExecutionUnit, FrameSlot, FunctionId, FunctionUnit, RegisterId, RegisterType,
    SIRInstruction, SIROffset, SIRTerminator, SIRValue,
};
#[cfg(test)]
use celox_state_layout::MemoryLayoutMode;
use celox_state_layout::{
    RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
    RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
    RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
    STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET, STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET,
};

use celox_state_layout::get_byte_size;

mod binary;
mod binary_mask;
mod call;
mod float;
mod frame;
mod load;
mod runtime_event;
mod sparse;
mod store;
mod terminator;
mod unary;
mod values;
mod wide_arith;
mod word;
use binary::*;
use binary_mask::*;
use call::*;
use float::*;
use frame::*;
use load::*;
use runtime_event::*;
use sparse::*;
use store::*;
use terminator::*;
use unary::*;
use values::*;
use wide_arith::*;
use word::*;

type AbsoluteAddr = StateAddr;
type RegionedAbsoluteAddr = celox_design::RegionedStateAddr;
type MemoryLayout = celox_state_layout::MemoryLayout<AbsoluteAddr>;

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
    /// The parameter holding the frame base address, in a function unit.
    frame_base: Option<u32>,
}

impl LocalAllocator {
    fn new() -> Self {
        Self {
            next: 0,
            reg_map: HashMap::default(),
            block_arg_locals: HashMap::default(),
            trigger_locals: HashMap::default(),
            num_locals: 0,
            frame_base: None,
        }
    }

    /// The locals of a function unit, whose only parameter, local 0, is the
    /// `i32` frame base address.
    fn for_function_unit() -> Self {
        Self {
            next: 1,
            num_locals: 1,
            frame_base: Some(0),
            ..Self::new()
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
/// - Imports `("env", "celox_extern_<func>")` with signature `(i64, ...) -> i64`
///   for every extern function `func` the units call; see [`extern_import_name`]
/// - Exports function `"run"` with signature `() -> i64`
pub fn compile_units(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
) -> WasmModule {
    compile_units_calling(units, &[], layout, four_state, emit_triggers)
}

/// [`compile_units`] for units that may call the program's `functions`.
///
/// A module whose units call a function embeds every function unit, as an
/// internal function `(i32 frame_base) -> i64` returning the unit's status:
/// function `k` has index `externs + 1 + k`, after the imports and `run`.
/// Its frame slots are addressed from `frame_base`, the absolute address of
/// its frame in the call stack ([`celox_state_layout::CallStackLayout`]). A
/// module whose units call nothing is the one [`compile_units`] builds.
pub fn compile_units_calling(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    functions: &[FunctionUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
) -> WasmModule {
    let calls = units
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .flat_map(|block| &block.instructions)
        .any(|instruction| matches!(instruction, SIRInstruction::Call { .. }));
    let functions = if calls { functions } else { &[] };
    let mut module = Module::new();
    let externs = ExternImports::collect(
        units
            .iter()
            .chain(functions.iter().map(|function| &function.unit)),
    );

    // Type section: () -> i64 for `run`, then one type per extern import,
    // then (i32) -> i64 for function units.
    let mut types = TypeSection::new();
    types.ty().function(vec![], vec![ValType::I64]);
    for &(_, arity) in &externs.functions {
        types
            .ty()
            .function(vec![ValType::I64; arity], vec![ValType::I64]);
    }
    let function_type = externs.functions.len() as u32 + 1;
    if !functions.is_empty() {
        types.ty().function(vec![ValType::I32], vec![ValType::I64]);
    }
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
    for (index, &(func, _)) in externs.functions.iter().enumerate() {
        imports.import(
            "env",
            &extern_import_name(func),
            EntityType::Function(index as u32 + 1),
        );
    }
    module.section(&imports);

    // Function section: `run` has type 0. Imported functions come first in
    // the function index space; function units follow `run`.
    let mut function_section = FunctionSection::new();
    function_section.function(0);
    for _ in functions {
        function_section.function(function_type);
    }
    module.section(&function_section);

    // Export section: export the function as "run"
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, externs.functions.len() as u32);
    module.section(&exports);

    let calls = Calls::new(functions, externs.functions.len() as u32 + 1, layout);

    // Code section
    let mut codes = CodeSection::new();
    let func = compile_function(
        units,
        layout,
        four_state,
        emit_triggers,
        &externs,
        &calls,
        LocalAllocator::new(),
    );
    codes.function(&func);
    for function in functions {
        let func = compile_function(
            std::slice::from_ref(&function.unit),
            layout,
            four_state,
            emit_triggers,
            &externs,
            &calls,
            LocalAllocator::for_function_unit(),
        );
        codes.function(&func);
    }
    module.section(&codes);

    WasmModule {
        bytes: module.finish(),
    }
}

/// The function units a module embeds, as its code sees them.
struct Calls<'a> {
    functions: &'a [FunctionUnit<RegionedAbsoluteAddr>],
    /// Every frame slot, by its address.
    slots: HashMap<RegionedAbsoluteAddr, &'a FrameSlot<RegionedAbsoluteAddr>>,
    /// The Wasm function index of function 0.
    first_index: u32,
    stack: Option<celox_state_layout::CallStackLayout>,
}

impl<'a> Calls<'a> {
    fn new(
        functions: &'a [FunctionUnit<RegionedAbsoluteAddr>],
        first_index: u32,
        layout: &MemoryLayout,
    ) -> Self {
        Self {
            functions,
            slots: functions
                .iter()
                .flat_map(|function| &function.frame.slots)
                .map(|slot| (slot.addr, slot))
                .collect(),
            first_index,
            stack: layout.call_stack,
        }
    }
}

/// The name of the host import that calls extern function `func`.
pub fn extern_import_name(func: u32) -> String {
    format!("celox_extern_{func}")
}

/// Extern functions called by the compiled units, imported in this order.
struct ExternImports {
    /// `(func, arity)` in ascending `func` order.
    functions: Vec<(u32, usize)>,
}

impl ExternImports {
    fn collect<'u>(
        units: impl IntoIterator<Item = &'u ExecutionUnit<RegionedAbsoluteAddr>>,
    ) -> Self {
        let mut arities = std::collections::BTreeMap::new();
        for instruction in units
            .into_iter()
            .flat_map(|unit| unit.blocks.values())
            .flat_map(|block| &block.instructions)
        {
            let helper = match instruction {
                SIRInstruction::Binary(_, _, BinaryOp::Float { op, precision }, _) => {
                    float_needs_helper(celox_sir::float::binary_code(*op, *precision))
                }
                SIRInstruction::Unary(_, UnaryOp::Float { op, precision }, _) => {
                    float_needs_helper(celox_sir::float::unary_code(*op, *precision))
                }
                _ => false,
            };
            if helper {
                arities.insert(celox_sir::float::FLOAT_HELPER_ID, 3);
            }
            if let SIRInstruction::ExternCall { func, args, .. } = instruction {
                let arity = *arities.entry(*func).or_insert(args.len());
                assert_eq!(
                    arity,
                    args.len(),
                    "design assembly gives extern function {func} one prototype"
                );
            }
        }
        Self {
            functions: arities.into_iter().collect(),
        }
    }

    /// The Wasm function index of the import for `func`.
    fn function_index(&self, func: u32) -> u32 {
        self.functions
            .binary_search_by_key(&func, |&(func, _)| func)
            .expect("every called extern function is imported") as u32
    }
}

/// Compile all execution units into a single WASM function body, whose
/// parameters `locals` starts after.
fn compile_function(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    externs: &ExternImports,
    calls: &Calls<'_>,
    mut locals: LocalAllocator,
) -> Function {
    let params = locals.next;

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
            externs,
            calls,
            &mut locals,
            &mut instrs,
        );
    }

    // Successful return
    instrs.push(Instruction::I64Const(0));
    instrs.push(Instruction::End);

    // Build the function with all locals but its parameters declared as i64.
    let mut func = Function::new(vec![(locals.num_locals - params, ValType::I64)]);
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
#[allow(clippy::too_many_arguments)]
fn compile_unit(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    externs: &ExternImports,
    calls: &Calls<'_>,
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
                externs,
                calls,
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

#[allow(clippy::too_many_arguments)]
fn compile_instruction(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
    emit_triggers: bool,
    externs: &ExternImports,
    calls: &Calls<'_>,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let target = match inst {
        SIRInstruction::Store(addr, _, width, _, _, _)
        | SIRInstruction::Commit(_, addr, _, width, _)
            if *width != 0 =>
        {
            Some(addr)
        }
        _ => None,
    };
    if let Some(offsets) = target.and_then(|addr| layout.trace_notification_offsets(addr)) {
        for offset in offsets {
            instrs.push(Instruction::I32Const(offset as i32));
            instrs.push(Instruction::I32Const(1));
            instrs.push(Instruction::I32Store8(wasm_encoder::MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }));
        }
    }
    match inst {
        SIRInstruction::Imm(dst, val) => {
            compile_imm(dst, val, unit, four_state, &*locals, instrs);
        }
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            if let BinaryOp::Float { op, precision } = op {
                compile_float(
                    *dst,
                    *lhs,
                    Some(*rhs),
                    celox_sir::float::binary_code(*op, *precision),
                    externs,
                    locals,
                    instrs,
                );
            } else {
                compile_binary(dst, lhs, op, rhs, unit, four_state, &mut *locals, instrs);
            }
        }
        SIRInstruction::Unary(dst, op, src) => {
            if let UnaryOp::Float { op, precision } = op {
                compile_float(
                    *dst,
                    *src,
                    None,
                    celox_sir::float::unary_code(*op, *precision),
                    externs,
                    locals,
                    instrs,
                );
            } else if matches!(op, UnaryOp::ToPlanes | UnaryOp::FromPlanes) {
                compile_planes(dst, op, src, locals, instrs);
            } else {
                compile_unary(dst, op, src, unit, four_state, locals, instrs);
            }
        }
        // A frame slot has no entry in the state layout, and its address may
        // share its object identity with a state object: resolve it before
        // any layout lookup.
        SIRInstruction::Load(dst, addr, offset, op_width) if addr.region == FRAME_REGION => {
            let slot = calls.slots[addr];
            let dst = locals.reg_map[dst].clone();
            let frame_base = locals
                .frame_base
                .expect("only function units access frames");
            match static_bits(offset) {
                Some(bits) => load_slot(
                    &dst,
                    Base::I32(frame_base),
                    slot,
                    bits,
                    *op_width,
                    four_state,
                    instrs,
                ),
                None => load_slot_dynamic(
                    &dst, frame_base, slot, offset, *op_width, four_state, locals, instrs,
                ),
            }
        }
        SIRInstruction::Load(dst, addr, offset, op_width) => {
            compile_load(
                dst, addr, offset, *op_width, layout, four_state, locals, instrs,
            );
        }
        SIRInstruction::Store(addr, offset, op_width, src, triggers, comb_capture_sites)
            if addr.region == FRAME_REGION =>
        {
            // Frame slots are not state: nothing observes their stores.
            debug_assert!(triggers.is_empty() && comb_capture_sites.is_empty());
            let slot = calls.slots[addr];
            let src = locals.reg_map[src].clone();
            let frame_base = locals
                .frame_base
                .expect("only function units access frames");
            match static_bits(offset) {
                Some(bits) => store_slot(
                    &src,
                    Base::I32(frame_base),
                    slot,
                    bits,
                    *op_width,
                    four_state,
                    locals,
                    instrs,
                ),
                None => store_slot_dynamic(
                    &src, frame_base, slot, offset, *op_width, four_state, locals, instrs,
                ),
            }
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
            if src_addr.region == SPARSE_WORKING_REGION {
                compile_sparse_commit(
                    src_addr, dst_addr, *op_width, layout, four_state, locals, instrs,
                );
            } else {
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
        SIRInstruction::Call {
            function,
            frame,
            args,
            outputs,
            dst,
            ..
        } => {
            compile_call(
                *function, *frame, args, outputs, *dst, unit, four_state, calls, locals, instrs,
            );
        }
        SIRInstruction::ExternCall { dst, func, args } => {
            compile_extern_call(
                *dst,
                externs.function_index(*func),
                args,
                unit,
                locals,
                instrs,
            );
        }
    }
}

#[cfg(test)]
mod call_tests;

#[cfg(test)]
mod bit_count_tests;
