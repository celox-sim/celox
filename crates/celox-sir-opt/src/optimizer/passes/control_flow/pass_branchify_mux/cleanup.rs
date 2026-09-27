//! Parameter-only block cleanup and terminator substitution.

use super::*;

pub(super) fn inline_param_only_jump_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        let (pred_counts, jump_preds) = predecessor_info(eu);
        let use_blocks = register_use_blocks(eu);
        let mut eligible = eu
            .blocks
            .keys()
            .copied()
            .filter(|&block_id| block_id != eu.entry_block_id)
            .filter(|block_id| param_only_replacement(eu, *block_id, &use_blocks).is_some())
            .filter(|block_id| {
                let jump_count = jump_preds.get(block_id).map_or(0, Vec::len);
                jump_count > 0 && pred_counts.get(block_id).copied().unwrap_or(0) == jump_count
            })
            .collect::<Vec<_>>();
        eligible.sort();

        if eligible.is_empty() {
            break;
        }

        // Do not remove adjacent candidates from the same predecessor
        // snapshot.  Given A -> B, removing A can create new predecessors of
        // B which are absent from `jump_preds`; removing B afterwards would
        // then leave those predecessors targeting a deleted block.  A greedy
        // independent set still removes a constant fraction of a long chain,
        // so the number of whole-CFG rebuilds remains logarithmic.
        let mut selected = HashSet::default();
        let eligible = eligible
            .into_iter()
            .filter(|block_id| {
                let adjacent_to_selected_predecessor = jump_preds
                    .get(block_id)
                    .is_some_and(|preds| preds.iter().any(|pred| selected.contains(pred)));
                let adjacent_to_selected_successor = match &eu.blocks[block_id].terminator {
                    SIRTerminator::Jump(target, _) => selected.contains(target),
                    SIRTerminator::Branch {
                        true_block,
                        false_block,
                        ..
                    } => selected.contains(&true_block.0) || selected.contains(&false_block.0),
                    SIRTerminator::Switch { cases, default, .. } => {
                        selected.contains(default)
                            || cases.iter().any(|case| selected.contains(&case.target))
                    }
                    SIRTerminator::Return | SIRTerminator::Error(_) => false,
                };
                if adjacent_to_selected_predecessor || adjacent_to_selected_successor {
                    false
                } else {
                    selected.insert(*block_id);
                    true
                }
            })
            .collect::<Vec<_>>();

        for block_id in eligible {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            let Some(replacement) = param_only_replacement(eu, block_id, &use_blocks) else {
                continue;
            };
            let Some(preds) = jump_preds.get(&block_id) else {
                continue;
            };
            let params = eu.blocks[&block_id].params.clone();
            for &pred_id in preds {
                if !eu.blocks.contains_key(&pred_id) {
                    continue;
                }
                let pred_args = match &eu.blocks[&pred_id].terminator {
                    SIRTerminator::Jump(target, args) if *target == block_id => args.clone(),
                    _ => continue,
                };
                let map = params
                    .iter()
                    .copied()
                    .zip(pred_args)
                    .collect::<HashMap<_, _>>();
                eu.blocks.get_mut(&pred_id).unwrap().terminator =
                    substitute_terminator(&replacement, &map);
            }
            eu.blocks.remove(&block_id);
        }
    }
}

fn param_only_replacement(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Option<SIRTerminator> {
    let block = eu.blocks.get(&block_id)?;
    if !block.instructions.is_empty() || block.params.is_empty() {
        return None;
    }
    if block.params.iter().any(|param| {
        use_blocks
            .get(param)
            .is_some_and(|uses| uses.iter().any(|use_block| *use_block != block_id))
    }) {
        return None;
    }
    match &block.terminator {
        SIRTerminator::Jump(_, _) | SIRTerminator::Branch { .. } | SIRTerminator::Switch { .. } => {
            Some(block.terminator.clone())
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => None,
    }
}

fn register_use_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, HashSet<BlockId>> {
    let mut result = HashMap::<RegisterId, HashSet<BlockId>>::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            for value in inst_uses(inst) {
                result.entry(value).or_default().insert(block.id);
            }
        }
        for value in terminator_uses(&block.terminator) {
            result.entry(value).or_default().insert(block.id);
        }
    }
    result
}

fn predecessor_info(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> (HashMap<BlockId, usize>, HashMap<BlockId, Vec<BlockId>>) {
    let mut pred_counts = HashMap::default();
    let mut jump_preds: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(dst, _) => {
                *pred_counts.entry(*dst).or_default() += 1;
                jump_preds.entry(*dst).or_default().push(block.id);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                *pred_counts.entry(true_block.0).or_default() += 1;
                *pred_counts.entry(false_block.0).or_default() += 1;
            }
            SIRTerminator::Switch { cases, default, .. } => {
                for case in cases {
                    *pred_counts.entry(case.target).or_default() += 1;
                }
                *pred_counts.entry(*default).or_default() += 1;
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for preds in jump_preds.values_mut() {
        preds.sort();
    }
    (pred_counts, jump_preds)
}

fn substitute_terminator(
    term: &SIRTerminator,
    map: &HashMap<RegisterId, RegisterId>,
) -> SIRTerminator {
    let replace = |reg: RegisterId| map.get(&reg).copied().unwrap_or(reg);
    match term {
        SIRTerminator::Jump(target, args) => {
            SIRTerminator::Jump(*target, args.iter().copied().map(replace).collect())
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => SIRTerminator::Branch {
            cond: replace(*cond),
            true_block: (
                true_block.0,
                true_block.1.iter().copied().map(replace).collect(),
            ),
            false_block: (
                false_block.0,
                false_block.1.iter().copied().map(replace).collect(),
            ),
        },
        SIRTerminator::Switch {
            selector,
            cases,
            default,
        } => SIRTerminator::Switch {
            selector: replace(*selector),
            cases: cases.clone(),
            default: *default,
        },
        SIRTerminator::Return => SIRTerminator::Return,
        SIRTerminator::Error(code) => SIRTerminator::Error(*code),
    }
}
