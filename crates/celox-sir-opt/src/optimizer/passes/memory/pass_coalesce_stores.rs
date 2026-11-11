use crate::HashMap;
use crate::HashSet;
use crate::PassOptions;
use crate::ir::*;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::block_opt::{aggregate_static_offset, store_concat_source};
use super::pass_manager::ExecutionUnitPass;

pub(in crate::optimizer) struct CoalesceStoresPass {
    pub element_widths: Arc<HashMap<RegionedAbsoluteAddr, usize>>,
    pub max_store_width: usize,
}

impl Default for CoalesceStoresPass {
    fn default() -> Self {
        Self {
            element_widths: Arc::default(),
            max_store_width: 64,
        }
    }
}

impl ExecutionUnitPass for CoalesceStoresPass {
    fn name(&self) -> &'static str {
        "coalesce_stores"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut reg_counter = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);

        for block in eu.blocks.values_mut() {
            coalesce_block(
                block,
                &mut eu.register_map,
                &mut reg_counter,
                &self.element_widths,
                self.max_store_width,
            );
        }
    }
}

/// A candidate Store instruction within a block.
struct StoreCandidate {
    addr: RegionedAbsoluteAddr,
    inst_index: usize,
    offset: usize,
    width: usize,
    src_reg: RegisterId,
    comb_capture_sites: Vec<u32>,
}

#[derive(Default)]
struct StoreGroup {
    candidates: Vec<StoreCandidate>,
    // Open groups have disjoint ranges. The predecessor of a new range's
    // end suffices to detect overlap without quadratic scans of large blocks.
    ranges: BTreeMap<usize, usize>,
}

fn coalesce_block(
    block: &mut BasicBlock<RegionedAbsoluteAddr>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
    element_widths: &HashMap<RegionedAbsoluteAddr, usize>,
    max_store_width: usize,
) {
    // Step 1: Collect Store groups, sealing on reads.
    //
    // groups: addr -> vec of candidates (current open group)
    // sealed_groups: completed groups ready for coalescing analysis
    let mut groups: HashMap<RegionedAbsoluteAddr, StoreGroup> = HashMap::default();
    let mut sealed_groups: Vec<Vec<StoreCandidate>> = Vec::new();

    for (i, inst) in block.instructions.iter().enumerate() {
        if inst.is_host_interaction() {
            for (_, group) in groups.drain() {
                if group.candidates.len() >= 2 {
                    sealed_groups.push(group.candidates);
                }
            }
        }
        match inst {
            SIRInstruction::Store(addr, SIROffset::Static(off), width, src, triggers, sites)
                if triggers.is_empty() && sites.is_empty() =>
            {
                let Some(end) = off.checked_add(*width) else {
                    seal_group(&mut groups, addr, &mut sealed_groups);
                    continue;
                };
                let overlaps = groups.get(addr).is_some_and(|group| {
                    group
                        .ranges
                        .range(..end)
                        .next_back()
                        .is_some_and(|(_, &previous_end)| previous_end > *off)
                });
                if overlaps {
                    seal_group(&mut groups, addr, &mut sealed_groups);
                }
                let group = groups.entry(*addr).or_default();
                group.ranges.insert(*off, end);
                group.candidates.push(StoreCandidate {
                    addr: *addr,
                    inst_index: i,
                    offset: *off,
                    width: *width,
                    src_reg: *src,
                    comb_capture_sites: sites.clone(),
                });
            }
            SIRInstruction::Load(_, addr, _, _) => {
                seal_group(&mut groups, addr, &mut sealed_groups);
            }
            SIRInstruction::Commit(src, dst, _, _, _) => {
                seal_group(&mut groups, src, &mut sealed_groups);
                seal_group(&mut groups, dst, &mut sealed_groups);
            }
            // Noncandidate writes can overlap an open run or expose its state.
            SIRInstruction::Store(addr, ..) => {
                seal_group(&mut groups, addr, &mut sealed_groups);
            }
            // A call may read or overwrite any part of the objects its
            // effects list, so their open runs end before it.
            other => match other.opaque_accessed_objects() {
                Some(objects) => {
                    for object in objects {
                        seal_group(&mut groups, object, &mut sealed_groups);
                    }
                }
                None => {
                    for (_, group) in groups.drain() {
                        if group.candidates.len() >= 2 {
                            sealed_groups.push(group.candidates);
                        }
                    }
                }
            },
        }
    }

    // Flush remaining open groups
    for (_, group) in groups.drain() {
        if group.candidates.len() >= 2 {
            sealed_groups.push(group.candidates);
        }
    }

    // Step 2: Find contiguous sub-runs in each group
    struct Replacement {
        removed_indices: Vec<usize>,
        anchor_index: usize,
        new_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    }

    let mut replacements: Vec<Replacement> = Vec::new();

    for mut group in sealed_groups {
        if group.len() < 2 {
            continue;
        }

        group.sort_by_key(|c| c.offset);

        let mut run_start = 0;
        while run_start < group.len() {
            let mut run_end = run_start;
            let mut scan_end = run_start;
            let mut expected = group[run_start].offset + group[run_start].width;
            while scan_end + 1 < group.len() {
                let next = &group[scan_end + 1];
                if next.offset != expected {
                    break;
                }
                scan_end += 1;
                expected += next.width;
                let aggregate_width = expected - group[run_start].offset;
                if aggregate_width > max_store_width {
                    break;
                }
                if aggregate_static_offset(
                    group[run_start].offset,
                    aggregate_width,
                    element_widths.get(&group[run_start].addr).copied(),
                )
                .is_some()
                {
                    run_end = scan_end;
                } else if element_widths
                    .get(&group[run_start].addr)
                    .is_some_and(|element_width| {
                        !group[run_start].offset.is_multiple_of(*element_width)
                    })
                {
                    break;
                }
            }

            let run_len = run_end - run_start + 1;
            if run_len >= 2 {
                let sub_run = &group[run_start..=run_end];
                let merged_lsb = sub_run[0].offset;
                let total_width: usize = sub_run.iter().map(|c| c.width).sum();
                debug_assert!(total_width <= max_store_width);
                let anchor_index = sub_run.iter().map(|c| c.inst_index).max().unwrap();
                let removed_indices: Vec<usize> = sub_run.iter().map(|c| c.inst_index).collect();

                // Allocate new register for Concat result
                *reg_counter += 1;
                while register_map.contains_key(&RegisterId(*reg_counter)) {
                    *reg_counter += 1;
                }
                let concat_reg = RegisterId(*reg_counter);
                register_map.insert(
                    concat_reg,
                    RegisterType::Bit {
                        width: total_width,
                        signed: false,
                    },
                );

                // Concat order: MSB first, so reverse the LSB-sorted order
                let mut new_instructions = Vec::new();
                let concat_args: Vec<RegisterId> = sub_run
                    .iter()
                    .rev()
                    .map(|c| {
                        store_concat_source(
                            c.src_reg,
                            c.width,
                            register_map,
                            reg_counter,
                            &mut new_instructions,
                        )
                    })
                    .collect();
                let mut comb_capture_sites = Vec::new();
                for site_id in sub_run
                    .iter()
                    .flat_map(|c| c.comb_capture_sites.iter().copied())
                {
                    if !comb_capture_sites.contains(&site_id) {
                        comb_capture_sites.push(site_id);
                    }
                }

                // Get the addr from the first candidate (all share the same addr)
                let addr = if let SIRInstruction::Store(addr, _, _, _, _, _) =
                    &block.instructions[sub_run[0].inst_index]
                {
                    *addr
                } else {
                    unreachable!()
                };

                let Some(offset) = aggregate_static_offset(
                    merged_lsb,
                    total_width,
                    element_widths.get(&addr).copied(),
                ) else {
                    run_start = run_end + 1;
                    continue;
                };
                new_instructions.extend([
                    SIRInstruction::Concat(concat_reg, concat_args),
                    SIRInstruction::Store(
                        addr,
                        offset,
                        total_width,
                        concat_reg,
                        Vec::new(),
                        comb_capture_sites,
                    ),
                ]);

                replacements.push(Replacement {
                    removed_indices,
                    anchor_index,
                    new_instructions,
                });
            }

            run_start = run_end + 1;
        }
    }

    if replacements.is_empty() {
        return;
    }

    // Step 4: Rebuild instruction vector
    let mut removed_set: HashSet<usize> = HashSet::default();
    // Map from anchor index -> new instructions to insert (replacing the anchor)
    let mut insert_map: HashMap<usize, Vec<SIRInstruction<RegionedAbsoluteAddr>>> =
        HashMap::default();

    for repl in replacements {
        for &idx in &repl.removed_indices {
            removed_set.insert(idx);
        }
        insert_map
            .entry(repl.anchor_index)
            .or_default()
            .extend(repl.new_instructions);
    }

    let old_instructions = std::mem::take(&mut block.instructions);
    let mut new_instructions = Vec::with_capacity(old_instructions.len());

    for (i, inst) in old_instructions.into_iter().enumerate() {
        if let Some(replacement) = insert_map.remove(&i) {
            // This is an anchor point: insert Concat + wide Store(s)
            new_instructions.extend(replacement);
            // The anchor itself is also a removed store — don't re-add it
        } else if !removed_set.contains(&i) {
            new_instructions.push(inst);
        }
    }

    block.instructions = new_instructions;
}

fn seal_group(
    groups: &mut HashMap<RegionedAbsoluteAddr, StoreGroup>,
    addr: &RegionedAbsoluteAddr,
    sealed_groups: &mut Vec<Vec<StoreCandidate>>,
) {
    if let Some(group) = groups.remove(addr) {
        if group.candidates.len() >= 2 {
            sealed_groups.push(group.candidates);
        }
    }
}

#[cfg(test)]
mod tests;
