use super::*;

fn emit_with_pages(
    pages: StatePageBases,
    emit: impl FnOnce(&mut VecAssembler<Aarch64Relocation>),
) -> JitCode {
    let mut ops = VecAssembler::<Aarch64Relocation>::new(0);
    // The sparse helpers use x16/x17/x30 and v0..v7. Save the ABI registers
    // in unused caller-saved registers for this standalone execution test.
    dynasm!(ops ; .arch aarch64 ; mov x14, x29 ; mov x15, x30);
    if let Some(page) = pages.primary {
        emit_address_to(&mut ops, STATE_PAGE_REG, STATE_REG, page);
    }
    for (register, page) in pages.secondary.into_iter().flatten() {
        emit_address_to(&mut ops, register, STATE_REG, page);
    }
    emit(&mut ops);
    dynasm!(ops ; .arch aarch64 ; mov x29, x14 ; mov x30, x15 ; mov x0, xzr ; ret);
    JitCode::new(&ops.finalize().unwrap()).unwrap()
}

fn word(state: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap())
}

fn reference_commit(state: &mut [u8], row: &[u64]) {
    let [
        src,
        dst,
        bytes,
        dirty,
        dirty_count,
        summary,
        summary_count,
        four_state,
    ] = row.try_into().unwrap();
    for summary_index in 0..summary_count as usize {
        let at = summary as usize + summary_index * 8;
        let bits = word(state, at);
        state[at..at + 8].fill(0);
        for bit in 0..64 {
            let dirty_index = summary_index * 64 + bit;
            if bits & (1 << bit) == 0 || dirty_index >= dirty_count as usize {
                continue;
            }
            let at = dirty as usize + dirty_index * 8;
            let chunks = word(state, at);
            state[at..at + 8].fill(0);
            for chunk_bit in 0..64 {
                let offset = (dirty_index * 64 + chunk_bit) * 8;
                if chunks & (1 << chunk_bit) == 0 || offset >= bytes as usize {
                    continue;
                }
                let len = 8.min(bytes as usize - offset);
                for plane in 0..if four_state != 0 { 2 } else { 1 } {
                    let delta = plane * bytes as usize + offset;
                    state.copy_within(
                        src as usize + delta..src as usize + delta + len,
                        dst as usize + delta,
                    );
                }
            }
        }
    }
}

fn layouts() -> impl Iterator<Item = (usize, StatePageBases)> {
    [0, 0x0100_0127].into_iter().flat_map(|bias| {
        [
            StatePageBases::default(),
            StatePageBases {
                primary: Some((bias + 36864) as i64),
                // Caller-saved page registers are sufficient for standalone
                // helpers and exercise positive and negative relative offsets.
                secondary: [
                    Some((1, (bias + 32768) as i64)),
                    Some((2, (bias + 16384) as i64)),
                    None,
                    None,
                ],
            },
        ]
        .into_iter()
        .map(move |pages| (bias, pages))
    })
}

#[test]
fn sparse_worklists_finish_each_active_entry_across_word_boundaries() {
    const CAPACITY: usize = 130;
    let rows = (0..CAPACITY)
        .map(|index| {
            vec![
                3 + index as u64 * 64,
                16387 + index as u64 * 64,
                1 + index as u64 % 17,
                32768 + index as u64 * 8,
                1,
                36864 + index as u64 * 8,
                1,
                index as u64 % 2,
            ]
        })
        .collect::<Vec<_>>();
    for (bias, pages) in layouts() {
        let descriptors = rows
            .iter()
            .flat_map(|row| {
                row.iter().enumerate().map(|(column, &value)| {
                    value
                        + if matches!(column, 0 | 1 | 3 | 5) {
                            bias as u64
                        } else {
                            0
                        }
                })
            })
            .collect::<Vec<_>>();
        let jit = emit_with_pages(pages, |ops| {
            emit_sparse_commit_worklist(ops, &descriptors, (bias + 40960) as i32, CAPACITY, pages)
                .unwrap()
        });
        for active in [0, 1, 1 << 63, (1 << 31) | 1, u64::MAX] {
            let mut state = (0..41000)
                .map(|index| (index as u8).wrapping_mul(73).wrapping_add(17))
                .collect::<Vec<_>>();
            for index in 0..CAPACITY {
                let dirty = [0_u64, 1, 2, u64::MAX][index % 4];
                let summary = [1_u64, 0, 2, u64::MAX][index / 4 % 4];
                state[32768 + index * 8..32776 + index * 8].copy_from_slice(&dirty.to_le_bytes());
                state[36864 + index * 8..36872 + index * 8].copy_from_slice(&summary.to_le_bytes());
            }
            for index in 0..3 {
                state[40960 + index * 8..40968 + index * 8].copy_from_slice(&active.to_le_bytes());
            }
            let mut expected = state.clone();
            expected[40960..40984].fill(0);
            for (index, row) in rows.iter().enumerate() {
                if active & (1 << (index % 64)) != 0 {
                    reference_commit(&mut expected, row);
                }
            }
            // Only the computed addresses are dereferenced; bias also tests
            // offsets too large for a pair of ADD-immediate instructions.
            assert_eq!(
                unsafe { (jit.fn_ptr)(state.as_mut_ptr().wrapping_sub(bias)) },
                0
            );
            assert_eq!(
                state, expected,
                "bias={bias} pages={pages:?} active={active:x}"
            );
        }
    }
}

#[test]
fn sparse_chunk_addresses_preserve_indices_tail_bytes_and_both_planes() {
    for (bias, pages) in layouts() {
        for bytes in [1, 7, 9, 17, 511, 513] {
            for four_state in [false, true] {
                let row = [3, 16387, bytes, 32768, 2, 36864, 2, u64::from(four_state)];
                let jit = emit_with_pages(pages, |ops| {
                    emit_sparse_commit(
                        ops,
                        (bias + 3) as i32,
                        (bias + 16387) as i32,
                        bytes as usize,
                        (bias + 32768) as i32,
                        2,
                        (bias + 36864) as i32,
                        2,
                        four_state,
                        pages,
                    )
                });
                for bits in [0_u64, 1, 2, 1 << 63, u64::MAX] {
                    let mut state = (0..37000)
                        .map(|index| (index as u8).wrapping_mul(73).wrapping_add(17))
                        .collect::<Vec<_>>();
                    for offset in [32768, 32776, 36864, 36872] {
                        state[offset..offset + 8].copy_from_slice(&bits.to_le_bytes());
                    }
                    let mut expected = state.clone();
                    reference_commit(&mut expected, &row);
                    assert_eq!(
                        unsafe { (jit.fn_ptr)(state.as_mut_ptr().wrapping_sub(bias)) },
                        0
                    );
                    assert_eq!(
                        state, expected,
                        "bias={bias} pages={pages:?} bytes={bytes} four_state={four_state} bits={bits:x}"
                    );
                }
            }
        }
    }
}
