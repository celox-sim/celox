//! Calls of function units ([`SIRInstruction::Call`]).
//!
//! A function unit is compiled once into a function `(mem_ptr, frame_ptr) ->
//! i64` of the target's default calling convention, which returns the
//! unit's status. Its frame slots are addressed from `frame_ptr`, the
//! absolute address of its frame in the call stack of the state image
//! ([`celox_state_layout::CallStackLayout`]).
//!
//! [`SIRInstruction::Call`]: crate::SIRInstruction::Call

use cranelift::codegen::ir::{FuncRef, MemFlagsData as MemFlags};
use cranelift::prelude::*;
use cranelift_frontend::FunctionBuilder;

use super::core::{TransValue, cast_type, get_chunk_as_i64, get_cl_type, promote_to_physical};
use super::memory::Placement;
use super::{SIRTranslator, TranslationState};
use crate::{
    FrameLayout, FrameSlot, FunctionId, HashMap, RegionedAbsoluteAddr, RegisterId, SIROffset,
};

/// The trap of a call whose frame does not fit in the stack.
pub const STACK_OVERFLOW_TRAP: TrapCode = TrapCode::user(1).unwrap();
/// The trap of a call nested deeper than [`crate::MAX_CALL_DEPTH`].
pub const CALL_DEPTH_TRAP: TrapCode = TrapCode::user(2).unwrap();

/// The function units of the program, as the translator sees them.
#[derive(Default)]
pub(crate) struct Callees {
    /// Each function's frame, by [`FunctionId`].
    pub(crate) frames: Vec<FrameLayout<RegionedAbsoluteAddr>>,
    /// Every frame slot of the program, by its address.
    pub(crate) slots: HashMap<RegionedAbsoluteAddr, FrameSlot<RegionedAbsoluteAddr>>,
    /// The functions called from the CLIF function being translated, as
    /// declared in it. The engine declares them before each translation.
    pub(crate) refs: HashMap<FunctionId, FuncRef>,
}

impl SIRTranslator {
    /// Translate function unit `unit` into a function `(mem_ptr, frame_ptr)
    /// -> i64`. It is translated whole: a caller's frame base cannot be
    /// handed to a chain of split chunks.
    pub fn translate_function(
        &self,
        unit: &crate::ExecutionUnit<RegionedAbsoluteAddr>,
        mut builder: FunctionBuilder,
    ) {
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let mem_ptr = builder.block_params(entry)[0];
        let frame_ptr = builder.block_params(entry)[1];
        self.translate_units_into(
            std::slice::from_ref(unit),
            &mut builder,
            mem_ptr,
            Some(frame_ptr),
            None,
        );
        builder.seal_all_blocks();
        builder.finalize(self.target_config);
    }

    /// Run `function` in the frame based `frame` bytes into the stack: guard
    /// the stack and the call depth, store the arguments, call it, end the
    /// running unit with its status when that is nonzero, and load `dst`
    /// from its output slots.
    pub(super) fn translate_call_inst(
        &self,
        state: &mut TranslationState,
        function: FunctionId,
        frame: RegisterId,
        args: &[(usize, RegisterId)],
        outputs: &[usize],
        dst: Option<RegisterId>,
    ) {
        let stack = self
            .layout
            .call_stack
            .expect("the layout of a program that calls functions has a call stack");
        let callee = &self.callees.frames[function.0 as usize];
        let func_ref = self.callees.refs[&function];
        let four_state = self.options.four_state;
        let ptr_type = self.target_config.pointer_type();

        // The frame must lie in the stack. Producers keep calls within it, so
        // a frame outside is a broken invariant, not a simulation error.
        let frame_width = state.register_map[&frame].width();
        let frame_value = state.regs[&frame].first_value(state.builder);
        let frame_offset = promote_to_physical(state, frame_value, frame_width, false, types::I64);
        let outside = match stack.size.checked_sub(callee.size) {
            Some(limit) => state.builder.ins().icmp_imm_u(
                IntCC::UnsignedGreaterThan,
                frame_offset,
                limit as i64,
            ),
            None => state.builder.ins().iconst(types::I8, 1),
        };
        state.builder.ins().trapnz(outside, STACK_OVERFLOW_TRAP);
        let frame_offset = cast_type(state.builder, frame_offset, ptr_type);
        let frame_ptr = state.builder.ins().iadd(state.mem_ptr, frame_offset);
        let frame_ptr = state
            .builder
            .ins()
            .iadd_imm_s(frame_ptr, stack.base_offset as i64);

        let depth = state.builder.ins().load(
            types::I64,
            MemFlags::trusted(),
            state.mem_ptr,
            stack.depth_offset as i32,
        );
        let too_deep = state.builder.ins().icmp_imm_u(
            IntCC::UnsignedGreaterThanOrEqual,
            depth,
            crate::MAX_CALL_DEPTH as i64,
        );
        state.builder.ins().trapnz(too_deep, CALL_DEPTH_TRAP);
        let deeper = state.builder.ins().iadd_imm_s(depth, 1);
        state.builder.ins().store(
            MemFlags::trusted(),
            deeper,
            state.mem_ptr,
            stack.depth_offset as i32,
        );

        for &(position, arg) in args {
            let slot = &callee.slots[position];
            let value = state.regs[&arg].clone();
            let width = state.register_map[&arg].width();
            self.translate_placed_store(
                state,
                &value,
                width,
                &SIROffset::Static(0),
                slot.width,
                Placement::slot(frame_ptr, slot, four_state),
            );
        }

        let call = state
            .builder
            .ins()
            .call(func_ref, &[state.mem_ptr, frame_ptr]);
        let status = state.builder.inst_results(call)[0];
        // The callee left the depth as it found it, also when it failed.
        state.builder.ins().store(
            MemFlags::trusted(),
            depth,
            state.mem_ptr,
            stack.depth_offset as i32,
        );
        let failed = state.builder.create_block();
        let returned = state.builder.create_block();
        state.builder.ins().brif(status, failed, &[], returned, &[]);
        state.builder.switch_to_block(failed);
        state.builder.seal_block(failed);
        state.builder.ins().return_(&[status]);
        state.builder.switch_to_block(returned);
        state.builder.seal_block(returned);

        let Some(dst) = dst else {
            return;
        };
        let width = state.register_map[&dst].width();
        let value = if let [position] = outputs {
            let slot = &callee.slots[*position];
            self.translate_placed_load(
                state,
                &SIROffset::Static(0),
                slot.width,
                width,
                Placement::slot(frame_ptr, slot, four_state),
            )
        } else {
            self.concatenate_outputs(state, callee, outputs, frame_ptr, width)
        };
        state.regs.insert(dst, value);
    }

    /// The concatenation of the `outputs` slots of the frame at `frame_ptr`,
    /// the first in the least significant bits, as a `width`-bit value.
    fn concatenate_outputs(
        &self,
        state: &mut TranslationState,
        callee: &FrameLayout<RegionedAbsoluteAddr>,
        outputs: &[usize],
        frame_ptr: Value,
        width: usize,
    ) -> TransValue {
        let four_state = self.options.four_state;
        let words = width.div_ceil(64).max(1);
        let zero = state.builder.ins().iconst(types::I64, 0);
        let mut values = vec![zero; words];
        let mut masks = vec![zero; words];
        // Ors `chunks`, each 64 bits of a value with no bits set above its
        // width, into `into` from bit `shift` on.
        let place = |builder: &mut FunctionBuilder,
                     into: &mut [Value],
                     chunks: &[Value],
                     slot_width: usize,
                     shift: usize| {
            for chunk in 0..slot_width.div_ceil(64) {
                let part = get_chunk_as_i64(builder, chunks, chunk);
                let bit = shift + chunk * 64;
                let (word, offset) = (bit / 64, bit % 64);
                let low = builder.ins().ishl_imm_u(part, offset as i64);
                into[word] = builder.ins().bor(into[word], low);
                if offset != 0 && word + 1 < into.len() {
                    let high = builder.ins().ushr_imm_u(part, (64 - offset) as i64);
                    into[word + 1] = builder.ins().bor(into[word + 1], high);
                }
            }
        };
        let mut shift = 0;
        for &position in outputs {
            let slot = &callee.slots[position];
            let value = self.translate_placed_load(
                state,
                &SIROffset::Static(0),
                slot.width,
                slot.width,
                Placement::slot(frame_ptr, slot, four_state),
            );
            let chunks = value.load_value_chunks(state.builder);
            place(state.builder, &mut values, &chunks, slot.width, shift);
            if let Some(chunks) = value.load_mask_chunks(state.builder) {
                place(state.builder, &mut masks, &chunks, slot.width, shift);
            }
            shift += slot.width;
        }
        if words == 1 {
            let ty = get_cl_type(width);
            values[0] = cast_type(state.builder, values[0], ty);
            masks[0] = cast_type(state.builder, masks[0], ty);
            values.truncate(1);
            masks.truncate(1);
        }
        if four_state {
            TransValue::FourState { values, masks }
        } else {
            TransValue::TwoState(values)
        }
    }
}
