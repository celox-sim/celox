//! Selection of the state pages whose bases a function keeps in registers.

use super::*;

pub(super) fn select_state_base_pages(
    function: &MFunction,
    secondary_registers: impl IntoIterator<Item = u8>,
) -> StatePageBases {
    let accesses = collect_state_page_accesses(function);
    let candidates = accesses
        .iter()
        .map(|access| access.offset().div_euclid(STATE_PAGE_BYTES) * STATE_PAGE_BYTES)
        .collect::<BTreeSet<_>>();
    let mut selected = Vec::new();
    let mut current_costs = accesses
        .iter()
        .copied()
        .map(state_page_baseline_cost)
        .collect::<Vec<_>>();

    // Greedily choose pages by the actual number of address-materialization
    // instructions they remove.  A page base can cover several adjacent
    // pages for wider accesses, so counting exact pages independently can
    // waste preserved registers on redundant bases.
    for _ in 0..=MAX_SECONDARY_STATE_PAGES {
        let mut best = None;
        for &candidate in &candidates {
            if selected.contains(&candidate) {
                continue;
            }
            let benefit = accesses
                .iter()
                .copied()
                .zip(current_costs.iter().copied())
                .map(|(access, current_cost)| {
                    state_page_cost(access, candidate)
                        .map_or(0, |cost| current_cost.saturating_sub(cost))
                })
                .sum::<usize>();
            if best
                .map(|(best_benefit, best_page)| {
                    benefit > best_benefit || (benefit == best_benefit && candidate < best_page)
                })
                .unwrap_or(true)
            {
                best = Some((benefit, candidate));
            }
        }
        let Some((benefit, page)) = best else {
            break;
        };
        if benefit < MIN_STATE_PAGE_BENEFIT {
            break;
        }
        selected.push(page);
        for (access, current_cost) in accesses.iter().copied().zip(&mut current_costs) {
            if let Some(cost) = state_page_cost(access, page) {
                *current_cost = (*current_cost).min(cost);
            }
        }
    }

    let secondary = secondary_registers
        .into_iter()
        .zip(selected.iter().skip(1))
        .take(MAX_SECONDARY_STATE_PAGES)
        .map(|(register, page)| Some((register, *page)))
        .chain(std::iter::repeat(None))
        .take(MAX_SECONDARY_STATE_PAGES)
        .collect::<Vec<_>>()
        .try_into()
        .expect("secondary state page count is fixed");
    StatePageBases {
        primary: selected.first().copied(),
        secondary,
        frame: None,
    }
}

pub(super) fn collect_state_page_accesses(function: &MFunction) -> Vec<StatePageAccess> {
    let mut accesses = Vec::new();
    for block in &function.blocks {
        for instruction in &block.insts {
            match instruction {
                MInst::Load {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Direct {
                    offset: i64::from(*offset),
                    size: *size,
                    store: false,
                }),
                MInst::Store {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Direct {
                    offset: i64::from(*offset),
                    size: *size,
                    store: true,
                }),
                MInst::LoadIndexed {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Indexed {
                    offset: i64::from(*offset),
                    size: *size,
                    store: false,
                }),
                MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => accesses.push(StatePageAccess::Indexed {
                    offset: i64::from(*offset),
                    size: *size,
                    store: true,
                }),
                MInst::OrStoreIndexed {
                    base: BaseReg::SimState,
                    offset,
                    size,
                    ..
                } => {
                    let offset = i64::from(*offset);
                    accesses.push(StatePageAccess::Indexed {
                        offset,
                        size: *size,
                        store: false,
                    });
                    accesses.push(StatePageAccess::Indexed {
                        offset,
                        size: *size,
                        store: true,
                    });
                }
                MInst::MemCopy {
                    src_offset,
                    dst_offset,
                    byte_len,
                } if mem_copy_uses_forward_vectors(*src_offset, *dst_offset, *byte_len) => {
                    accesses.push(StatePageAccess::Vector {
                        offset: i64::from(*src_offset),
                    });
                    accesses.push(StatePageAccess::Vector {
                        offset: i64::from(*dst_offset),
                    });
                }
                MInst::MemFill { dst_offset, .. } => {
                    accesses.push(StatePageAccess::Vector {
                        offset: i64::from(*dst_offset),
                    });
                }
                MInst::BranchPred {
                    predicate:
                        BranchPredicate::MemoryNonZero {
                            base: BaseReg::SimState,
                            offset,
                            size,
                        },
                    ..
                } => accesses.push(StatePageAccess::Direct {
                    offset: i64::from(*offset),
                    size: *size,
                    store: false,
                }),
                MInst::SparseMarkActive {
                    active_index,
                    active_bits_offset,
                    ..
                } => {
                    let offset = i64::from(*active_bits_offset) + i64::from(*active_index / 64) * 8;
                    accesses.push(StatePageAccess::Direct {
                        offset,
                        size: OpSize::S64,
                        store: false,
                    });
                    accesses.push(StatePageAccess::Direct {
                        offset,
                        size: OpSize::S64,
                        store: true,
                    });
                }
                MInst::PackedLaneCompare {
                    offset,
                    lane_count,
                    element_stride,
                    rhs,
                    ..
                } => collect_packed_lane_accesses(
                    &mut accesses,
                    *offset,
                    *lane_count,
                    *element_stride,
                    *rhs,
                ),
                _ => {}
            }
        }
    }
    accesses
}

pub(super) fn collect_packed_lane_accesses(
    accesses: &mut Vec<StatePageAccess>,
    offset: i32,
    lane_count: u8,
    element_stride: u8,
    rhs: PackedLaneCompareRhs,
) {
    let Some(size) = packed_lane_size(element_stride) else {
        return;
    };
    let lane_count = usize::from(lane_count);
    let element_stride = usize::from(element_stride);
    if lane_count * element_stride % 16 == 0 {
        let lanes_per_vector = 16 / element_stride;
        for lane_base in (0..lane_count).step_by(lanes_per_vector) {
            let delta =
                i64::try_from(lane_base * element_stride).expect("packed lane offset fits in i64");
            accesses.push(StatePageAccess::Vector {
                offset: i64::from(offset) + delta,
            });
            if let PackedLaneCompareRhs::Memory {
                offset: rhs_offset, ..
            } = rhs
            {
                accesses.push(StatePageAccess::Vector {
                    offset: i64::from(rhs_offset) + delta,
                });
            }
        }
    } else {
        for lane in 0..lane_count {
            let delta =
                i64::try_from(lane * element_stride).expect("packed lane offset fits in i64");
            accesses.push(StatePageAccess::Direct {
                offset: i64::from(offset) + delta,
                size,
                store: false,
            });
            if let PackedLaneCompareRhs::Memory {
                offset: rhs_offset, ..
            } = rhs
            {
                accesses.push(StatePageAccess::Direct {
                    offset: i64::from(rhs_offset) + delta,
                    size,
                    store: false,
                });
            }
        }
    }
}

pub(super) fn packed_lane_size(element_stride: u8) -> Option<OpSize> {
    match element_stride {
        1 => Some(OpSize::S8),
        2 => Some(OpSize::S16),
        4 => Some(OpSize::S32),
        _ => None,
    }
}

pub(super) fn mem_copy_uses_forward_vectors(
    src_offset: i32,
    dst_offset: i32,
    byte_len: usize,
) -> bool {
    if byte_len == 0 || src_offset == dst_offset {
        return false;
    }
    let byte_len = byte_len as i64;
    let src_end = i64::from(src_offset) + byte_len;
    let dst_end = i64::from(dst_offset) + byte_len;
    let copy_backward = src_end > i64::from(dst_offset)
        && dst_end > i64::from(src_offset)
        && dst_offset > src_offset;
    !copy_backward && (16..=256).contains(&byte_len)
}

pub(super) fn state_page_baseline_cost(access: StatePageAccess) -> usize {
    match access {
        StatePageAccess::Direct {
            offset,
            size,
            store,
        } => scalar_memory_offset_cost(offset, size, store),
        StatePageAccess::Indexed {
            offset,
            size,
            store,
        } => indexed_access_cost(offset, size, store),
        StatePageAccess::Vector { offset } => address_materialization_cost(offset),
    }
}

pub(super) fn state_page_cost(access: StatePageAccess, page: i64) -> Option<usize> {
    let offset = access.offset() - page;
    match access {
        StatePageAccess::Direct { size, store, .. } => {
            Some(scalar_memory_offset_cost(offset, size, store))
        }
        StatePageAccess::Indexed { size, store, .. } => indexed_offset_cost(offset, size, store),
        StatePageAccess::Vector { .. } => Some(address_materialization_cost(offset)),
    }
}

pub(super) fn indexed_access_cost(offset: i64, size: OpSize, store: bool) -> usize {
    indexed_offset_cost(offset, size, store).unwrap_or_else(|| address_materialization_cost(offset))
}

pub(super) fn indexed_offset_cost(offset: i64, size: OpSize, store: bool) -> Option<usize> {
    if memory_access_encoding(SCRATCH0, SCRATCH0, offset, size, store).is_some() {
        Some(0)
    } else if split_memory_offset(offset, size, store).is_some()
        || add_sub_immediate(offset).is_some()
    {
        Some(1)
    } else if add_sub_immediate_pair(offset).is_some() {
        Some(2)
    } else {
        None
    }
}
