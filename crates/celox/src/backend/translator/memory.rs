use cranelift::prelude::*;

use super::core::{TransValue, cast_type, get_chunk_as_i64, get_cl_type};
use super::{SIRTranslator, TranslationState, get_byte_size};
use crate::ir::{RegionedAbsoluteAddr, RegisterId, SIROffset, STABLE_REGION, TriggerIdWithKind};

impl SIRTranslator {
    pub(super) fn translate_load_inst(
        &self,
        state: &mut TranslationState,
        dst: &RegisterId,
        addr: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        op_width: &usize,
    ) {
        let d_phys_width = state.register_map[dst].width();

        // 1. Calculate base memory address
        let abs = addr.absolute_addr();
        let (mem_base, base_offset_bytes) = if addr.region == STABLE_REGION {
            (state.mem_ptr, self.layout.offsets[&abs])
        } else {
            (
                state.mem_ptr,
                self.layout.working_base_offset
                    + *self.layout.working_offsets.get(&abs).unwrap_or_else(|| {
                        panic!(
                            "Working region address is not laid out: region={}, addr={}",
                            addr.region, abs
                        )
                    }),
            )
        };

        if let SIROffset::Static(val) = offset {
            let byte_off = (val >> 3) as i64;
            let bit_shift = val & 7;
            let static_addr = state
                .builder
                .ins()
                .iadd_imm(mem_base, (base_offset_bytes as i64) + byte_off);

            if bit_shift == 0 {
                if d_phys_width <= 64 && matches!(*op_width, 8 | 16 | 32 | 64) {
                    let v = self.translate_load_native_aligned(
                        state,
                        static_addr,
                        *op_width,
                        d_phys_width,
                    );
                    if self.options.four_state {
                        let m = if self.layout.is_4states[&abs] {
                            let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                            let static_addr_m = state
                                .builder
                                .ins()
                                .iadd_imm(static_addr, var_byte_size as i64);
                            self.translate_load_native_aligned(
                                state,
                                static_addr_m,
                                *op_width,
                                d_phys_width,
                            )
                        } else {
                            state.builder.ins().iconst(types::I64, 0)
                        };
                        state.regs.insert(
                            *dst,
                            TransValue::FourState {
                                values: vec![v],
                                masks: vec![m],
                            },
                        );
                    } else {
                        state.regs.insert(*dst, TransValue::TwoState(vec![v]));
                    }
                    return;
                }

                if d_phys_width > 64 && (*op_width).is_multiple_of(64) {
                    let chunks_v = self.translate_load_multi_word_aligned_words(
                        state,
                        static_addr,
                        *op_width,
                        d_phys_width,
                    );
                    if self.options.four_state {
                        let chunks_m = if self.layout.is_4states[&abs] {
                            let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                            let static_addr_m = state
                                .builder
                                .ins()
                                .iadd_imm(static_addr, var_byte_size as i64);
                            self.translate_load_multi_word_aligned_words(
                                state,
                                static_addr_m,
                                *op_width,
                                d_phys_width,
                            )
                        } else {
                            let zero_chunk = state.builder.ins().iconst(types::I64, 0);
                            vec![zero_chunk; d_phys_width.div_ceil(64)]
                        };
                        state.regs.insert(
                            *dst,
                            TransValue::FourState {
                                values: chunks_v,
                                masks: chunks_m,
                            },
                        );
                    } else {
                        state.regs.insert(*dst, TransValue::TwoState(chunks_v));
                    }
                    return;
                }
            }
        }

        let (byte_offset_val, bit_shift_val) = match offset {
            SIROffset::Static(val) => {
                let byte_off = (val >> 3) as i64;
                let bit_shift = (val & 7) as i64;
                (
                    state.builder.ins().iconst(types::I64, byte_off),
                    state.builder.ins().iconst(types::I64, bit_shift),
                )
            }
            SIROffset::Dynamic(reg) => {
                let total_bit_offset =
                    cast_type(state.builder, state.regs[reg].values()[0], types::I64);
                (
                    state.builder.ins().ushr_imm(total_bit_offset, 3),
                    state.builder.ins().band_imm(total_bit_offset, 7),
                )
            }
        };

        // Physical read-start byte address
        let final_addr = state
            .builder
            .ins()
            .iadd_imm(mem_base, base_offset_bytes as i64);
        let final_addr = state.builder.ins().iadd(final_addr, byte_offset_val);

        // 3. Dispatch functions based on physical width
        let res_chunks_v = if d_phys_width <= 64 {
            vec![self.translate_load_native(
                state,
                final_addr,
                bit_shift_val,
                *op_width,
                d_phys_width,
            )]
        } else {
            self.translate_load_multi_word(
                state,
                final_addr,
                bit_shift_val,
                *op_width,
                d_phys_width,
            )
        };

        if self.options.four_state {
            let is_var_4state = self.layout.is_4states[&abs];
            let res_chunks_m = if is_var_4state {
                let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                let final_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(final_addr, var_byte_size as i64);
                if d_phys_width <= 64 {
                    vec![self.translate_load_native(
                        state,
                        final_addr_m,
                        bit_shift_val,
                        *op_width,
                        d_phys_width,
                    )]
                } else {
                    self.translate_load_multi_word(
                        state,
                        final_addr_m,
                        bit_shift_val,
                        *op_width,
                        d_phys_width,
                    )
                }
            } else {
                let zero_chunk = state.builder.ins().iconst(types::I64, 0);
                if d_phys_width <= 64 {
                    vec![zero_chunk]
                } else {
                    vec![zero_chunk; d_phys_width.div_ceil(64)]
                }
            };
            state.regs.insert(
                *dst,
                TransValue::FourState {
                    values: res_chunks_v,
                    masks: res_chunks_m,
                },
            );
        } else {
            state.regs.insert(*dst, TransValue::TwoState(res_chunks_v));
        }
    }

    pub(super) fn translate_store_inst(
        &self,
        state: &mut TranslationState,
        addr: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        op_width: &usize,
        src_reg: &RegisterId,
        triggers: &[TriggerIdWithKind],
    ) {
        // Get physical register definition information
        let s_phys_width = state.register_map[src_reg].width();

        // 1. Calculate base memory address (static offset)
        let abs = addr.absolute_addr();
        let (mem_base, base_offset_bytes) = if addr.region == STABLE_REGION {
            (state.mem_ptr, self.layout.offsets[&abs])
        } else {
            (
                state.mem_ptr,
                self.layout.working_base_offset
                    + *self.layout.working_offsets.get(&abs).unwrap_or_else(|| {
                        panic!(
                            "Working region address is not laid out: region={}, addr={}",
                            addr.region, abs
                        )
                    }),
            )
        };

        let (byte_offset_val, bit_shift_val) = match offset {
            SIROffset::Static(val) => {
                let byte_off = (val >> 3) as i64;
                let bit_shift = (val & 7) as i64;
                (
                    state.builder.ins().iconst(types::I64, byte_off),
                    state.builder.ins().iconst(types::I64, bit_shift),
                )
            }
            SIROffset::Dynamic(reg) => {
                let total_bit_offset =
                    cast_type(state.builder, state.regs[reg].values()[0], types::I64);
                (
                    state.builder.ins().ushr_imm(total_bit_offset, 3),
                    state.builder.ins().band_imm(total_bit_offset, 7),
                )
            }
        };

        // Physical write-start byte address
        let final_addr = state
            .builder
            .ins()
            .iadd_imm(mem_base, base_offset_bytes as i64);
        let final_addr = state.builder.ins().iadd(final_addr, byte_offset_val);

        // 3. Dispatch functions based on physical width
        let v_chunks = state.regs[src_reg].values().to_vec();
        let m_chunks = if self.options.four_state {
            match state.regs[src_reg].masks() {
                Some(m) => m.to_vec(),
                None => {
                    // Source register is TwoState (e.g. from block params before full 4-state support)
                    // Generate zero masks for each value chunk
                    let s_phys_ty = get_cl_type(s_phys_width);
                    v_chunks
                        .iter()
                        .map(|_| state.builder.ins().iconst(s_phys_ty, 0))
                        .collect()
                }
            }
        } else {
            vec![]
        };

        if s_phys_width <= 64 {
            // Handle single register (i8 ~ i128)
            let val_v = v_chunks[0];
            let no_rmw_width = matches!(*op_width, 8 | 16 | 32 | 64);
            let is_static_aligned = matches!(offset, SIROffset::Static(v) if v & 7 == 0);
            if no_rmw_width && is_static_aligned {
                self.translate_store_native_aligned(state, final_addr, *op_width, val_v);
                if self.options.four_state && self.layout.is_4states[&abs] {
                    let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                    let final_addr_m = state
                        .builder
                        .ins()
                        .iadd_imm(final_addr, var_byte_size as i64);
                    self.translate_store_native_aligned(
                        state,
                        final_addr_m,
                        *op_width,
                        m_chunks[0],
                    );
                }
            } else {
                self.translate_store_native(state, final_addr, bit_shift_val, *op_width, val_v);
                if self.options.four_state && self.layout.is_4states[&abs] {
                    let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                    let final_addr_m = state
                        .builder
                        .ins()
                        .iadd_imm(final_addr, var_byte_size as i64);
                    self.translate_store_native(
                        state,
                        final_addr_m,
                        bit_shift_val,
                        *op_width,
                        m_chunks[0],
                    );
                }
            }
        } else {
            // Handle multiple registers (i64 * n)
            let is_static_aligned = matches!(offset, SIROffset::Static(v) if v & 7 == 0);
            let full_word_store = (*op_width).is_multiple_of(64);
            if is_static_aligned && full_word_store {
                self.translate_store_multi_word_aligned_words(
                    state, final_addr, *op_width, &v_chunks,
                );
                if self.options.four_state && self.layout.is_4states[&abs] {
                    let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                    let final_addr_m = state
                        .builder
                        .ins()
                        .iadd_imm(final_addr, var_byte_size as i64);
                    self.translate_store_multi_word_aligned_words(
                        state,
                        final_addr_m,
                        *op_width,
                        &m_chunks,
                    );
                }
            } else {
                self.translate_store_multi_word_from_chunks(
                    state,
                    final_addr,
                    bit_shift_val,
                    *op_width,
                    &v_chunks,
                );
                if self.options.four_state && self.layout.is_4states[&abs] {
                    let var_byte_size = get_byte_size(self.layout.widths[&abs]);
                    let final_addr_m = state
                        .builder
                        .ins()
                        .iadd_imm(final_addr, var_byte_size as i64);
                    self.translate_store_multi_word_from_chunks(
                        state,
                        final_addr_m,
                        bit_shift_val,
                        *op_width,
                        &m_chunks,
                    );
                }
            }
        }

        // 4-state type boundary enforcement:
        // When storing to a 2-state variable, replace the source register's mask with zeros.
        // This is critical because the combinational analysis inlines intermediate variables,
        // making subsequent LogicPaths that share expression nodes (via lower_cache) reuse
        // the same registers. Without this, X masks would propagate through 2-state variables.
        if self.options.four_state && !self.layout.is_4states[&abs] {
            let zero_masks: Vec<_> = v_chunks
                .iter()
                .map(|_| state.builder.ins().iconst(get_cl_type(s_phys_width), 0))
                .collect();
            state.regs.insert(
                *src_reg,
                TransValue::FourState {
                    values: v_chunks,
                    masks: zero_masks,
                },
            );
        }
        // 5. Trigger detection
        self.translate_trigger_detection(state, addr, offset, op_width, triggers);
    }

    pub(super) fn translate_commit_inst(
        &self,
        state: &mut TranslationState,
        src_addr: &RegionedAbsoluteAddr,
        dst_addr: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        op_width: &usize,
        triggers: &[TriggerIdWithKind],
    ) {
        let src_abs = src_addr.absolute_addr();
        let (src_mem_base, src_base_offset_bytes) = if src_addr.region == STABLE_REGION {
            (state.mem_ptr, self.layout.offsets[&src_abs])
        } else {
            (
                state.mem_ptr,
                self.layout.working_base_offset
                    + *self
                        .layout
                        .working_offsets
                        .get(&src_abs)
                        .unwrap_or_else(|| {
                            panic!(
                                "Working region address is not laid out: region={}, addr={}",
                                src_addr.region, src_abs
                            )
                        }),
            )
        };

        let dst_abs = dst_addr.absolute_addr();
        let (dst_mem_base, dst_base_offset_bytes) = if dst_addr.region == STABLE_REGION {
            (state.mem_ptr, self.layout.offsets[&dst_abs])
        } else {
            (
                state.mem_ptr,
                self.layout.working_base_offset
                    + *self
                        .layout
                        .working_offsets
                        .get(&dst_abs)
                        .unwrap_or_else(|| {
                            panic!(
                                "Working region address is not laid out: region={}, addr={}",
                                dst_addr.region, dst_abs
                            )
                        }),
            )
        };

        // Fast path: byte-aligned static commit can be lowered to direct memory copies.
        // This avoids generating complex RMW-style store sequences.
        if let SIROffset::Static(bit_off) = offset
            && bit_off % 8 == 0
            && op_width.is_multiple_of(8)
        {
            let byte_off = bit_off / 8;
            let byte_len = get_byte_size(*op_width);

            let src_addr_val = state
                .builder
                .ins()
                .iadd_imm(src_mem_base, (src_base_offset_bytes + byte_off) as i64);
            let dst_addr_val = state
                .builder
                .ins()
                .iadd_imm(dst_mem_base, (dst_base_offset_bytes + byte_off) as i64);

            self.translate_copy_bytes(state, src_addr_val, dst_addr_val, byte_len);

            if self.options.four_state {
                let var_byte_size_src = get_byte_size(self.layout.widths[&src_abs]);
                let var_byte_size_dst = get_byte_size(self.layout.widths[&dst_abs]);
                let src_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(src_addr_val, var_byte_size_src as i64);
                let dst_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(dst_addr_val, var_byte_size_dst as i64);
                self.translate_copy_bytes(state, src_addr_m, dst_addr_m, byte_len);
            }
            return;
        }

        let (byte_offset_val, bit_shift_val) = match offset {
            SIROffset::Static(val) => {
                let byte_off = (val >> 3) as i64;
                let bit_shift = (val & 7) as i64;
                (
                    state.builder.ins().iconst(types::I64, byte_off),
                    state.builder.ins().iconst(types::I64, bit_shift),
                )
            }
            SIROffset::Dynamic(reg) => {
                let total_bit_offset =
                    cast_type(state.builder, state.regs[reg].values()[0], types::I64);
                (
                    state.builder.ins().ushr_imm(total_bit_offset, 3),
                    state.builder.ins().band_imm(total_bit_offset, 7),
                )
            }
        };

        let src_final_addr = state
            .builder
            .ins()
            .iadd_imm(src_mem_base, src_base_offset_bytes as i64);
        let src_final_addr = state.builder.ins().iadd(src_final_addr, byte_offset_val);

        let dst_final_addr = state
            .builder
            .ins()
            .iadd_imm(dst_mem_base, dst_base_offset_bytes as i64);
        let dst_final_addr = state.builder.ins().iadd(dst_final_addr, byte_offset_val);

        let phys_width = self.layout.widths[&src_abs];
        if phys_width <= 64 {
            let val = self.translate_load_native(
                state,
                src_final_addr,
                bit_shift_val,
                *op_width,
                phys_width,
            );
            self.translate_store_native(state, dst_final_addr, bit_shift_val, *op_width, val);

            if self.options.four_state {
                let var_byte_size_src = get_byte_size(self.layout.widths[&src_abs]);
                let var_byte_size_dst = get_byte_size(self.layout.widths[&dst_abs]);
                let src_final_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(src_final_addr, var_byte_size_src as i64);
                let dst_final_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(dst_final_addr, var_byte_size_dst as i64);
                let val_m = self.translate_load_native(
                    state,
                    src_final_addr_m,
                    bit_shift_val,
                    *op_width,
                    phys_width,
                );
                self.translate_store_native(
                    state,
                    dst_final_addr_m,
                    bit_shift_val,
                    *op_width,
                    val_m,
                );
            }
        } else {
            let chunks = self.translate_load_multi_word(
                state,
                src_final_addr,
                bit_shift_val,
                *op_width,
                phys_width,
            );
            self.translate_store_multi_word_from_chunks(
                state,
                dst_final_addr,
                bit_shift_val,
                *op_width,
                &chunks,
            );

            if self.options.four_state {
                let var_byte_size_src = get_byte_size(self.layout.widths[&src_abs]);
                let var_byte_size_dst = get_byte_size(self.layout.widths[&dst_abs]);
                let src_final_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(src_final_addr, var_byte_size_src as i64);
                let dst_final_addr_m = state
                    .builder
                    .ins()
                    .iadd_imm(dst_final_addr, var_byte_size_dst as i64);
                let chunks_m = self.translate_load_multi_word(
                    state,
                    src_final_addr_m,
                    bit_shift_val,
                    *op_width,
                    phys_width,
                );
                self.translate_store_multi_word_from_chunks(
                    state,
                    dst_final_addr_m,
                    bit_shift_val,
                    *op_width,
                    &chunks_m,
                );
            }
        }
        // 2. Trigger detection
        self.translate_trigger_detection(state, dst_addr, offset, op_width, triggers);
    }

    fn translate_copy_bytes(
        &self,
        state: &mut TranslationState,
        src_addr: Value,
        dst_addr: Value,
        byte_len: usize,
    ) {
        let mut offset = 0usize;

        while offset + 8 <= byte_len {
            let v = state
                .builder
                .ins()
                .load(types::I64, MemFlags::new(), src_addr, offset as i32);
            state
                .builder
                .ins()
                .store(MemFlags::new(), v, dst_addr, offset as i32);
            offset += 8;
        }

        let rem = byte_len - offset;
        if rem >= 4 {
            let v = state
                .builder
                .ins()
                .load(types::I32, MemFlags::new(), src_addr, offset as i32);
            state
                .builder
                .ins()
                .store(MemFlags::new(), v, dst_addr, offset as i32);
            offset += 4;
        }
        let rem = byte_len - offset;
        if rem >= 2 {
            let v = state
                .builder
                .ins()
                .load(types::I16, MemFlags::new(), src_addr, offset as i32);
            state
                .builder
                .ins()
                .store(MemFlags::new(), v, dst_addr, offset as i32);
            offset += 2;
        }
        if byte_len - offset >= 1 {
            let v = state
                .builder
                .ins()
                .load(types::I8, MemFlags::new(), src_addr, offset as i32);
            state
                .builder
                .ins()
                .store(MemFlags::new(), v, dst_addr, offset as i32);
        }
    }

    fn translate_store_native(
        &self,
        state: &mut TranslationState,
        addr: Value,      // byte_offset already added
        bit_shift: Value, // 0 ~ 7 (i64)
        op_width: usize,  // bit width to write
        src_val: Value,   // source
    ) {
        // 1. Determine minimum necessary access type
        let total_bits_needed = op_width + 7;
        let access_ty = if total_bits_needed <= 8 {
            types::I8
        } else if total_bits_needed <= 16 {
            types::I16
        } else if total_bits_needed <= 32 {
            types::I32
        } else {
            types::I64
        };
        let m_raw = if op_width >= 64 {
            !0u64
        } else {
            (1u64 << op_width) - 1
        };
        let mask_val = state.builder.ins().iconst(access_ty, m_raw as i64);

        // 3. Align types
        let src_aligned = cast_type(state.builder, src_val, access_ty);
        let shift_amt = cast_type(state.builder, bit_shift, access_ty);

        // 4. RMW
        let old_val = state
            .builder
            .ins()
            .load(access_ty, MemFlags::new(), addr, 0);

        // Shift mask by bit_shift
        let shifted_mask = state.builder.ins().ishl(mask_val, shift_amt);
        let inv_mask = state.builder.ins().bnot(shifted_mask);

        // Shift source by bit_shift and mask it
        let shifted_src = state.builder.ins().ishl(src_aligned, shift_amt);
        let masked_src = state.builder.ins().band(shifted_src, shifted_mask);

        // Composing
        let preserved_part = state.builder.ins().band(old_val, inv_mask);
        let combined = state.builder.ins().bor(masked_src, preserved_part);

        state
            .builder
            .ins()
            .store(MemFlags::new(), combined, addr, 0);
    }

    fn translate_store_native_aligned(
        &self,
        state: &mut TranslationState,
        addr: Value,
        op_width: usize,
        src_val: Value,
    ) {
        let ty = match op_width {
            8 => types::I8,
            16 => types::I16,
            32 => types::I32,
            64 => types::I64,
            _ => unreachable!("aligned native store width must be 8/16/32/64"),
        };

        let v = cast_type(state.builder, src_val, ty);
        state.builder.ins().store(MemFlags::new(), v, addr, 0);
    }

    fn translate_load_native(
        &self,
        state: &mut TranslationState,
        addr: Value,
        bit_shift: Value,
        op_width: usize,
        d_phys_width: usize,
    ) -> Value {
        // 1. Determine minimum necessary access type
        let total_bits_needed = op_width + 7;
        let access_ty = if total_bits_needed <= 8 {
            types::I8
        } else if total_bits_needed <= 16 {
            types::I16
        } else if total_bits_needed <= 32 {
            types::I32
        } else {
            types::I64
        };

        // 2. Load and alignment (right shift)
        let raw_val = state
            .builder
            .ins()
            .load(access_ty, MemFlags::new(), addr, 0);
        let shift_amt = cast_type(state.builder, bit_shift, access_ty);
        let aligned_val = state.builder.ins().ushr(raw_val, shift_amt);

        // 3. Mask with logical width

        let m_raw = if op_width >= 64 {
            !0u64 // All bits 1 (0xFFFFFFFF_FFFFFFFF)
        } else {
            (1u64 << op_width) - 1
        };
        let masked_val = state.builder.ins().band_imm(aligned_val, m_raw as i64);

        // 4. Cast to physical register type and return
        cast_type(state.builder, masked_val, get_cl_type(d_phys_width))
    }

    fn translate_load_native_aligned(
        &self,
        state: &mut TranslationState,
        addr: Value,
        op_width: usize,
        d_phys_width: usize,
    ) -> Value {
        let ty = match op_width {
            8 => types::I8,
            16 => types::I16,
            32 => types::I32,
            64 => types::I64,
            _ => unreachable!("aligned native load width must be 8/16/32/64"),
        };

        let raw = state.builder.ins().load(ty, MemFlags::new(), addr, 0);
        cast_type(state.builder, raw, get_cl_type(d_phys_width))
    }

    fn translate_load_multi_word_aligned_words(
        &self,
        state: &mut TranslationState,
        addr: Value,
        op_width: usize,
        d_phys_width: usize,
    ) -> Vec<Value> {
        let num_phys_chunks = d_phys_width.div_ceil(64);
        let needed_logic_chunks = op_width / 64;
        let mut res_chunks = Vec::with_capacity(num_phys_chunks);

        for i in 0..num_phys_chunks {
            if i < needed_logic_chunks {
                let v = state
                    .builder
                    .ins()
                    .load(types::I64, MemFlags::new(), addr, (i * 8) as i32);
                res_chunks.push(v);
            } else {
                res_chunks.push(state.builder.ins().iconst(types::I64, 0));
            }
        }

        res_chunks
    }

    fn translate_load_multi_word(
        &self,
        state: &mut TranslationState,
        addr: Value,
        bit_shift: Value,
        op_width: usize,
        d_phys_width: usize,
    ) -> Vec<Value> {
        let bit_shift_i64 = cast_type(state.builder, bit_shift, types::I64);
        let inv_bit_shift = state.builder.ins().irsub_imm(bit_shift_i64, 64);
        let has_bit_shift = state.builder.ins().icmp_imm(IntCC::NotEqual, bit_shift, 0);

        let num_phys_chunks = d_phys_width.div_ceil(64);
        let needed_logic_chunks = op_width.div_ceil(64);
        let mut res_chunks = Vec::with_capacity(num_phys_chunks);

        for i in 0..num_phys_chunks {
            if i < needed_logic_chunks {
                let cur_mem =
                    state
                        .builder
                        .ins()
                        .load(types::I64, MemFlags::new(), addr, (i * 8) as i32);
                let nxt_mem = state.builder.ins().load(
                    types::I64,
                    MemFlags::new(),
                    addr,
                    ((i + 1) * 8) as i32,
                );

                // Slide-combine: Extract bits across two adjacent chunks to handle unaligned access.
                // low = cur >> shift, high = nxt << (64 - shift)
                let low = state.builder.ins().ushr(cur_mem, bit_shift_i64);
                let high = state.builder.ins().ishl(nxt_mem, inv_bit_shift);
                let combined = state.builder.ins().bor(low, high);

                // If shift is 0, we can skip the combination and use the current chunk directly.
                let val = state.builder.ins().select(has_bit_shift, combined, cur_mem);

                // Masking with op_width for the last chunk
                let is_last_valid = i == needed_logic_chunks - 1;
                let remaining_bits = op_width % 64;
                let final_chunk = if is_last_valid && remaining_bits > 0 {
                    let mask = (1u64 << remaining_bits) - 1;
                    state.builder.ins().band_imm(val, mask as i64)
                } else {
                    val
                };
                res_chunks.push(final_chunk);
            } else {
                // Zero-fill remaining physical width
                res_chunks.push(state.builder.ins().iconst(types::I64, 0));
            }
        }
        res_chunks
    }

    fn translate_store_multi_word_aligned_words(
        &self,
        state: &mut TranslationState,
        addr: Value,
        op_width: usize,
        chunks: &[Value],
    ) {
        let words = op_width / 64;
        for i in 0..words {
            let chunk = get_chunk_as_i64(state.builder, chunks, i);
            let v = cast_type(state.builder, chunk, types::I64);
            state
                .builder
                .ins()
                .store(MemFlags::new(), v, addr, (i * 8) as i32);
        }
    }

    fn translate_store_multi_word_from_chunks(
        &self,
        state: &mut TranslationState,
        addr: Value,
        bit_shift: Value,
        op_width: usize,
        chunks: &[Value],
    ) {
        let bit_shift_i64 = cast_type(state.builder, bit_shift, types::I64);
        let inv_bit_shift = state.builder.ins().irsub_imm(bit_shift_i64, 64);
        let has_bit_shift = state
            .builder
            .ins()
            .icmp_imm(IntCC::NotEqual, bit_shift_i64, 0);
        let total_end_bit = state.builder.ins().iadd_imm(bit_shift_i64, op_width as i64);
        let max_dst_chunks = (op_width + 7).div_ceil(64);

        for i in 0..max_dst_chunks {
            // Dynamic guard: Is this chunk within write range?
            let chunk_start_bit = (i * 64) as i64;
            let is_in_range = state.builder.ins().icmp_imm(
                IntCC::UnsignedGreaterThan,
                total_end_bit,
                chunk_start_bit,
            );

            let write_block = state.builder.create_block();
            let next_block = state.builder.create_block();
            state
                .builder
                .ins()
                .brif(is_in_range, write_block, &[], next_block, &[]);

            state.builder.switch_to_block(write_block);
            let cur_src = get_chunk_as_i64(state.builder, chunks, i);
            let prev_src = if i > 0 {
                Some(get_chunk_as_i64(state.builder, chunks, i - 1))
            } else {
                None
            }
            .unwrap_or_else(|| state.builder.ins().iconst(types::I64, 0));
            // Perform slide-combine for the source value.
            let x = state.builder.ins().ishl(cur_src, bit_shift_i64);
            let y = state.builder.ins().ushr(prev_src, inv_bit_shift);
            let x = state.builder.ins().bor(x, y);
            let val = state.builder.ins().select(has_bit_shift, x, cur_src);

            let chunk_addr = state.builder.ins().iadd_imm(addr, (i * 8) as i64);
            self.perform_rmw_i64_dynamic(state, chunk_addr, val, i, bit_shift_i64, total_end_bit);

            state.builder.ins().jump(next_block, &[]);
            state.builder.switch_to_block(next_block);
            state.builder.seal_block(write_block);
            state.builder.seal_block(next_block);
        }
    }

    fn perform_rmw_i64_dynamic(
        &self,
        state: &mut TranslationState,
        addr: Value,
        val: Value,
        chunk_idx: usize,
        bit_shift: Value,     // % 8 of total offset
        total_end_bit: Value, // bit_shift + op_width
    ) {
        let chunk_start_bit = (chunk_idx * 64) as i64;

        // --- Create Mask ---
        // 1. Start mask: Keeps bits from the target bit_shift onwards.
        let m1 = state.builder.ins().iconst(types::I64, -1);
        let start_mask = state.builder.ins().ishl(m1, bit_shift);

        // 2. End mask: Keeps bits up to the total logical width.
        // mask = (1 << (total_end_bit - chunk_start_bit)) - 1
        let rel_end = state
            .builder
            .ins()
            .iadd_imm(total_end_bit, -chunk_start_bit);
        // Ensure shift_amt is within [0, 63] for Cranelift instructions.
        let shift_amt = state.builder.ins().band_imm(rel_end, 63);
        let end_mask = state.builder.ins().ishl(m1, shift_amt);
        let end_mask = state.builder.ins().bnot(end_mask);
        // If the write range covers the entire 64-bit chunk, no end mask is needed.
        let is_past_end =
            state
                .builder
                .ins()
                .icmp_imm(IntCC::SignedGreaterThanOrEqual, rel_end, 64);
        let end_mask = state.builder.ins().select(is_past_end, m1, end_mask);

        // Combined mask (start & end)
        let final_mask = if chunk_idx == 0 {
            state.builder.ins().band(start_mask, end_mask)
        } else {
            end_mask
        };

        // If this chunk is outside the write range (past the entire range), set mask to 0
        let is_in_range = state
            .builder
            .ins()
            .icmp_imm(IntCC::SignedGreaterThan, rel_end, 0);
        let zero = state.builder.ins().iconst(types::I64, 0);
        let final_mask = state.builder.ins().select(is_in_range, final_mask, zero);

        // --- Execute RMW ---
        let old = state
            .builder
            .ins()
            .load(types::I64, MemFlags::new(), addr, 0);
        let masked_val = state.builder.ins().band(val, final_mask);
        let inv_mask = state.builder.ins().bnot(final_mask);
        let preserved_old = state.builder.ins().band(old, inv_mask);
        let combined = state.builder.ins().bor(masked_val, preserved_old);

        state
            .builder
            .ins()
            .store(MemFlags::new(), combined, addr, 0);
    }
    fn translate_trigger_detection(
        &self,
        state: &mut TranslationState,
        addr: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        op_width: &usize,
        triggers: &[TriggerIdWithKind],
    ) {
        if triggers.is_empty() || !self.options.emit_triggers {
            return;
        }

        let abs = addr.absolute_addr();

        // 1. Extract old value from pre-loaded register
        let pre_loaded = state.trigger_old_values[&(abs, addr.region)];
        let mask = if *op_width >= 64 {
            !0u64
        } else {
            (1u64 << op_width) - 1
        };

        let old_val = match offset {
            SIROffset::Static(v) => {
                if *v == 0 {
                    state.builder.ins().band_imm(pre_loaded, mask as i64)
                } else {
                    let shifted = state.builder.ins().ushr_imm(pre_loaded, *v as i64);
                    state.builder.ins().band_imm(shifted, mask as i64)
                }
            }
            SIROffset::Dynamic(reg) => {
                let total_bit_offset =
                    cast_type(state.builder, state.regs[reg].values()[0], types::I64);
                let shifted = state.builder.ins().ushr(pre_loaded, total_bit_offset);
                let mask_val = state.builder.ins().iconst(types::I64, mask as i64);
                state.builder.ins().band(shifted, mask_val)
            }
        };

        // 2. Load new value from live memory
        let (_mem_base, base_offset_bytes) = if addr.region == STABLE_REGION {
            (state.mem_ptr, self.layout.offsets[&abs])
        } else {
            (
                state.mem_ptr,
                self.layout.working_base_offset
                    + *self.layout.working_offsets.get(&abs).unwrap_or_else(|| {
                        panic!(
                            "Working region address is not laid out: region={}, addr={}",
                            addr.region, abs
                        )
                    }),
            )
        };

        let (byte_offset_val, bit_shift_val) = match offset {
            SIROffset::Static(val) => {
                let byte_off = (val >> 3) as i64;
                let bit_shift = (val & 7) as i64;
                (
                    state.builder.ins().iconst(types::I64, byte_off),
                    state.builder.ins().iconst(types::I64, bit_shift),
                )
            }
            SIROffset::Dynamic(reg) => {
                let total_bit_offset =
                    cast_type(state.builder, state.regs[reg].values()[0], types::I64);
                (
                    state.builder.ins().ushr_imm(total_bit_offset, 3),
                    state.builder.ins().band_imm(total_bit_offset, 7),
                )
            }
        };

        let actual_final_addr = state
            .builder
            .ins()
            .iadd_imm(state.mem_ptr, base_offset_bytes as i64);
        let actual_final_addr = state.builder.ins().iadd(actual_final_addr, byte_offset_val);
        let new_val =
            self.translate_load_native(state, actual_final_addr, bit_shift_val, *op_width, 64);

        // 3. For each trigger, generate edge detection logic
        for trigger in triggers {
            let triggered = match trigger.kind {
                crate::ir::DomainKind::ClockPosedge => {
                    let c1 = state.builder.ins().icmp_imm(IntCC::Equal, old_val, 0);
                    let c2 = state.builder.ins().icmp_imm(IntCC::Equal, new_val, 1);
                    state.builder.ins().band(c1, c2)
                }
                crate::ir::DomainKind::ClockNegedge => {
                    let c1 = state.builder.ins().icmp_imm(IntCC::Equal, old_val, 1);
                    let c2 = state.builder.ins().icmp_imm(IntCC::Equal, new_val, 0);
                    state.builder.ins().band(c1, c2)
                }
                crate::ir::DomainKind::ResetAsyncHigh => {
                    state.builder.ins().icmp_imm(IntCC::Equal, new_val, 1)
                }
                crate::ir::DomainKind::ResetAsyncLow => {
                    state.builder.ins().icmp_imm(IntCC::Equal, new_val, 0)
                }
                crate::ir::DomainKind::Other => {
                    state.builder.ins().icmp(IntCC::NotEqual, old_val, new_val)
                }
            };

            // 4. Update triggered bits bitset
            let triggered_block = state.builder.create_block();
            let merge_block = state.builder.create_block();

            state
                .builder
                .ins()
                .brif(triggered, triggered_block, &[], merge_block, &[]);

            state.builder.switch_to_block(triggered_block);
            let byte_idx = trigger.id / 8;
            let bit_idx = trigger.id % 8;
            let bit_ptr = state.builder.ins().iadd_imm(
                state.mem_ptr,
                (self.layout.triggered_bits_offset + byte_idx) as i64,
            );
            let old_byte = state
                .builder
                .ins()
                .load(types::I8, MemFlags::new(), bit_ptr, 0);
            let set_bit = state
                .builder
                .ins()
                .iconst(types::I8, (1u8 << bit_idx) as i64);
            let new_byte = state.builder.ins().bor(old_byte, set_bit);
            state
                .builder
                .ins()
                .store(MemFlags::new(), new_byte, bit_ptr, 0);
            state.builder.ins().jump(merge_block, &[]);

            state.builder.switch_to_block(merge_block);
            state.builder.seal_block(triggered_block);
            state.builder.seal_block(merge_block);
        }
    }
}
