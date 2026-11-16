//! Initial state of a [`Simulator`]: initial values, memory contents and plusargs.

use super::*;

pub(super) fn write_bits_to_memory(
    mem: &mut [u8],
    dst_bit_offset: usize,
    bit_width: usize,
    src: &[u8],
) {
    write_bits_to_memory_from(mem, dst_bit_offset, bit_width, src, 0);
}

pub(super) fn write_bits_to_memory_from(
    mem: &mut [u8],
    dst_bit_offset: usize,
    bit_width: usize,
    src: &[u8],
    src_bit_offset: usize,
) {
    for bit in 0..bit_width {
        let src_bit_index = src_bit_offset + bit;
        let src_bit = (src[src_bit_index / 8] >> (src_bit_index % 8)) & 1;
        let dst_idx = (dst_bit_offset + bit) / 8;
        let dst_mask = 1u8 << ((dst_bit_offset + bit) % 8);
        if src_bit == 0 {
            mem[dst_idx] &= !dst_mask;
        } else {
            mem[dst_idx] |= dst_mask;
        }
    }
}

pub(super) fn write_initial_run_to_plane(
    mem: &mut [u8],
    signal: SignalRef,
    mask_plane: bool,
    run: &InitialMemoryWriteRun,
    src: &[u8],
) {
    let Some(array) = signal.array_layout else {
        let plane_size = signal.width.div_ceil(8);
        let plane_bit_offset =
            (signal.offset + usize::from(mask_plane) * plane_size) * 8 + run.bit_offset;
        write_bits_to_memory(mem, plane_bit_offset, run.bit_width, src);
        return;
    };

    let plane_offset = signal.offset + usize::from(mask_plane) * array.plane_size;
    let mut consumed = 0usize;
    while consumed < run.bit_width {
        let logical_offset = run.bit_offset + consumed;
        let element = logical_offset / array.element_width;
        let intra_element = logical_offset % array.element_width;
        let part_width = (run.bit_width - consumed).min(array.element_width - intra_element);
        let destination_bit_offset =
            (plane_offset + element * array.element_stride) * 8 + intra_element;

        if consumed.is_multiple_of(8)
            && destination_bit_offset.is_multiple_of(8)
            && part_width.is_multiple_of(8)
        {
            let src_byte = consumed / 8;
            let dst_byte = destination_bit_offset / 8;
            let byte_width = part_width / 8;
            mem[dst_byte..dst_byte + byte_width]
                .copy_from_slice(&src[src_byte..src_byte + byte_width]);
        } else {
            write_bits_to_memory_from(mem, destination_bit_offset, part_width, src, consumed);
        }
        consumed += part_width;
    }
}

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn apply_initial_values(&mut self) {
        let mut applied = false;
        let initial_memory_values = self.program.design.take_initial_state();
        for init in &initial_memory_values {
            applied = true;
            let signal = self.backend.resolve_signal(&init.address);
            match &init.data {
                InitialMemoryData::Packed {
                    value,
                    mask,
                    written_mask,
                } if signal.array_layout.is_some() => {
                    // An unpacked array is written element by element.
                    let runs = InitialMemoryData::packed_write_runs(
                        value,
                        mask,
                        written_mask,
                        signal.width,
                        self.backend.layout().four_state && signal.is_4state,
                    );
                    self.apply_initial_memory_writes(signal, &runs);
                }
                InitialMemoryData::Packed {
                    value,
                    mask,
                    written_mask,
                } => {
                    let width_mask = if signal.width == 0 {
                        BigUint::default()
                    } else {
                        (BigUint::from(1u8) << signal.width) - BigUint::from(1u8)
                    };
                    let preserve_mask = &width_mask ^ (written_mask & &width_mask);
                    let (current_value, current_mask) = self.backend.get_four_state(signal);
                    let value = (current_value & &preserve_mask) | (value & written_mask);
                    let mask = (current_mask & &preserve_mask) | (mask & written_mask);
                    if self.backend.layout().four_state && signal.is_4state {
                        self.backend.set_four_state(signal, value, mask);
                    } else {
                        let known_mask = &width_mask ^ (&mask & &width_mask);
                        self.backend.set_wide(signal, value & known_mask);
                    }
                }
                InitialMemoryData::Writes(runs) => {
                    self.apply_initial_memory_writes(signal, runs);
                }
            }
        }
        if applied {
            self.dirty = true;
        }
        self.program
            .design
            .restore_initial_state(initial_memory_values);
    }

    /// Set the simulation's plusargs (IEEE 1800-2023 21.6). The tool's
    /// command line, which UVM reads through `uvm_dpi_get_next_arg_c`,
    /// is the program name followed by them, each with the leading `+`
    /// that UVM recognizes plusargs by and that the builder lets callers
    /// leave out.
    pub(crate) fn set_plusargs(&mut self, plusargs: Vec<String>) {
        self.backend.heap_mut().set_tool_arguments(
            std::iter::once("celox".to_owned())
                .chain(plusargs.iter().map(|argument| {
                    if argument.starts_with('+') {
                        argument.clone()
                    } else {
                        format!("+{argument}")
                    }
                }))
                .collect(),
        );
        self.plusargs = celox_runtime::plusargs::Plusargs(plusargs);
    }

    pub(crate) fn run_plusargs_request(
        &mut self,
        site: &celox_design::HostRequestSite<SignalRef>,
        _timestamp: u64,
    ) -> Result<(), celox_runtime::Halt> {
        let query_signal = site.args[0].id;
        let (query_value, query_mask) = self.backend.get_four_state(query_signal);
        if query_mask != BigUint::default() {
            return Err(celox_runtime::Halt::Error(
                crate::RuntimeErrorCode::Runtime {
                    message: "plusargs query contains X or Z bits".into(),
                    signals: Vec::new(),
                },
            ));
        }
        let query = query_value.to_bytes_be();
        let query = query
            .iter()
            .position(|byte| *byte != 0)
            .map_or(&[][..], |start| &query[start..]);
        let matched = if site.operation == 0 {
            self.plusargs.test(query)
        } else if site.results[1].value_kind == celox_design::ValueKind::StrRef {
            let text = self.plusargs.text(query).map_err(|message| {
                celox_runtime::Halt::Error(crate::RuntimeErrorCode::Runtime {
                    message,
                    signals: Vec::new(),
                })
            })?;
            match text {
                Some(text) => {
                    let text = text.to_vec();
                    let handle = self.backend.heap_mut().intern(&text);
                    self.backend.set(site.results[1].id, handle);
                    true
                }
                None => false,
            }
        } else {
            let target = site.results[1].id;
            match self
                .plusargs
                .value(query, target.width)
                .map_err(|message| {
                    celox_runtime::Halt::Error(crate::RuntimeErrorCode::Runtime {
                        message,
                        signals: Vec::new(),
                    })
                })? {
                Some((value, mask)) => {
                    if self.backend.layout().four_state && target.is_4state {
                        self.backend.set_four_state(target, value, mask);
                    } else {
                        let width_mask = (BigUint::from(1u8) << target.width) - BigUint::from(1u8);
                        self.backend
                            .set_wide(target, value & (&width_mask ^ (mask & &width_mask)));
                    }
                    true
                }
                None => false,
            }
        };
        self.backend
            .set_wide(site.results[0].id, BigUint::from(u8::from(matched)));
        Ok(())
    }

    pub(crate) fn apply_testbench_memory_writes(
        &mut self,
        signal: SignalRef,
        runs: &[InitialMemoryWriteRun],
    ) {
        self.apply_initial_memory_writes(signal, runs);
        self.dirty = true;
    }

    pub(super) fn apply_initial_memory_writes(
        &mut self,
        signal: SignalRef,
        runs: &[InitialMemoryWriteRun],
    ) {
        let value_byte_size = signal.width.div_ceil(8);
        let write_mask = self.backend.layout().four_state && signal.is_4state;
        // Initial writes precede the first full dump; no mutable view escapes.
        let (ptr, mem_len) = self.backend.memory_as_ptr();
        let ptr = ptr.cast_mut();
        let mem = unsafe { std::slice::from_raw_parts_mut(ptr, mem_len) };

        for run in runs {
            if run.bit_width == 0 {
                continue;
            }
            // Two-state storage has no mask plane to hold an X or a Z.
            let run = if write_mask {
                std::borrow::Cow::Borrowed(run)
            } else {
                run.two_state()
            };
            let run = &*run;
            if signal.array_layout.is_some() {
                write_initial_run_to_plane(mem, signal, false, run, &run.value_bytes);
                if write_mask {
                    write_initial_run_to_plane(mem, signal, true, run, &run.mask_bytes);
                }
                continue;
            }
            if run.bit_offset % 8 == 0 && run.bit_width % 8 == 0 {
                let byte_offset = run.bit_offset / 8;
                let byte_width = run.bit_width / 8;
                let value_offset = signal.offset + byte_offset;
                mem[value_offset..value_offset + byte_width]
                    .copy_from_slice(&run.value_bytes[..byte_width]);
                if write_mask {
                    let mask_offset = signal.offset + value_byte_size + byte_offset;
                    mem[mask_offset..mask_offset + byte_width]
                        .copy_from_slice(&run.mask_bytes[..byte_width]);
                }
                continue;
            }

            write_bits_to_memory(
                mem,
                signal.offset * 8 + run.bit_offset,
                run.bit_width,
                &run.value_bytes,
            );
            if write_mask {
                write_bits_to_memory(
                    mem,
                    (signal.offset + value_byte_size) * 8 + run.bit_offset,
                    run.bit_width,
                    &run.mask_bytes,
                );
            }
        }
        self.backend.mark_settle_range(signal.offset, 1);
    }
}
