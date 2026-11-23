//! Turn a dispatch over scalar state loads into a lookup of their physical offsets.
use super::*;

struct Plan {
    dispatch: usize,
    index: VReg,
    first: usize,
    offsets: Vec<i32>,
    size: OpSize,
    load: usize,
    constant: Option<usize>,
    join: usize,
    arms: HashSet<BlockId>,
}

fn plan(
    func: &MFunction,
    dispatch: usize,
    positions: &HashMap<BlockId, usize>,
    shared: &HashSet<BlockId>,
) -> Option<Plan> {
    let parent = &func.blocks[dispatch];
    let MInst::JumpTable { index, targets, .. } = parent.terminator()? else {
        return None;
    };
    if !(8..=256).contains(&targets.len()) {
        return None;
    }
    let arms: HashSet<_> = targets.iter().copied().collect();
    let arm = |id: BlockId| &func.blocks[positions[&id]];
    let MInst::Jump { target: join_id } = *arm(targets[0]).terminator()? else {
        return None;
    };
    let join = positions[&join_id];
    let [phi] = func.blocks[join].phis.as_slice() else {
        return None;
    };
    if phi.sources.len() != arms.len() {
        return None;
    }
    // Each private arm contributes exactly its own load or literal to the phi.
    // Removing an arm then removes neither an effect nor a live-out value.
    let sources: HashMap<_, _> = phi.sources.iter().copied().collect();
    let mut loads = HashMap::default();
    let mut size = None;
    let mut constant = None;
    for &id in &arms {
        let block = arm(id);
        if !block.phis.is_empty() || shared.contains(&id) || id == func.blocks[0].id {
            return None;
        }
        let [value, MInst::Jump { target }] = block.insts.as_slice() else {
            return None;
        };
        if *target != join_id || value.def()? != *sources.get(&id)? {
            return None;
        }
        match *value {
            MInst::Load {
                base: BaseReg::SimState,
                offset,
                size: s,
                ..
            } if offset >= 0 => {
                if size.replace(s).is_some_and(|prev| prev != s) {
                    return None;
                }
                loads.insert(id, offset);
            }
            MInst::LoadImm { value, .. } => {
                if constant.replace(value).is_some_and(|prev| prev != value) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if loads.len() < 8 {
        return None;
    }
    let first = targets.iter().position(|id| loads.contains_key(id))?;
    let last = targets.iter().rposition(|id| loads.contains_key(id))?;
    // One unsigned range check must cover precisely the loads. A hole or a
    // second fallback value requires the original dispatch.
    let offsets = targets[first..=last]
        .iter()
        .map(|id| loads.get(id).copied())
        .collect::<Option<Vec<_>>>()?;
    Some(Plan {
        dispatch,
        index: *index,
        first,
        offsets,
        size: size?,
        load: positions[&targets[first]],
        constant: targets
            .iter()
            .find(|id| !loads.contains_key(id))
            .map(|id| positions[id]),
        join,
        arms,
    })
}

impl Plan {
    fn apply(self, func: &mut MFunction, dead: &mut HashSet<BlockId>) {
        let load = func.blocks[self.load].id;
        let fallback = self.constant.map(|i| func.blocks[i].id);
        let min = *self.offsets.iter().min().unwrap();
        let span = (*self.offsets.iter().max().unwrap() - min) as usize;
        let entry = if span <= u8::MAX as usize {
            OpSize::S8
        } else if span <= u16::MAX as usize {
            OpSize::S16
        } else {
            OpSize::S32
        };
        let bytes = entry.bytes() as usize;
        // The native constant pool stores u64 words in little-endian order.
        // Pack deltas from the minimum home so different ports can share the table.
        let mut words = vec![0u64; (self.offsets.len() * bytes).div_ceil(8)];
        for (i, offset) in self.offsets.iter().enumerate() {
            words[i * bytes / 8] |= ((*offset - min) as u64) << ((i * bytes % 8) * 8);
        }
        let table = func.intern_constant_table(words);
        let mut temp = || alloc_transient_vreg(&mut func.vregs, &mut func.spill_descs);
        let parent = &mut func.blocks[self.dispatch];
        parent.insts.pop();
        let key = if self.first == 0 {
            self.index
        } else {
            let dst = temp();
            parent.push(MInst::SubImm {
                dst,
                src: self.index,
                imm: self.first as i32,
            });
            dst
        };
        if let Some(false_bb) = fallback {
            parent.push(MInst::BranchPred {
                predicate: BranchPredicate::CompareImm {
                    lhs: key,
                    imm: self.offsets.len() as i32,
                    kind: CmpKind::LtU,
                },
                true_bb: load,
                false_bb,
            });
        } else {
            parent.push(MInst::Jump { target: load });
        }
        let block = &mut func.blocks[self.load];
        block.insts.clear();
        let table_ptr = temp();
        block.push(MInst::LoadConstantTableAddr {
            dst: table_ptr,
            table,
        });
        let byte_index = if bytes == 1 {
            key
        } else {
            let dst = temp();
            block.push(MInst::ShlImm {
                dst,
                src: key,
                imm: bytes.trailing_zeros() as u8,
            });
            dst
        };
        let offset = temp();
        block.push(MInst::LoadPtrIndexed {
            dst: offset,
            ptr: table_ptr,
            offset: 0,
            index: byte_index,
            size: entry,
        });
        // The old load can have spill metadata for its fixed home. The new
        // result depends on the selector and needs its own temporary.
        let result = temp();
        block.push(MInst::LoadIndexed {
            dst: result,
            base: BaseReg::SimState,
            offset: min,
            index: offset,
            scale: 1,
            size: self.size,
            alias_range: MemoryAliasRange::new(min, span + self.size.bytes() as usize),
        });
        let join = func.blocks[self.join].id;
        func.blocks[self.load].push(MInst::Jump { target: join });
        let phi = &mut func.blocks[self.join].phis[0];
        phi.sources.retain(|(id, _)| Some(*id) == fallback);
        phi.sources.push((load, result));
        dead.extend(
            self.arms
                .into_iter()
                .filter(|&id| id != load && Some(id) != fallback),
        );
    }
}

pub(super) fn run(func: &mut MFunction) {
    let dispatches: Vec<_> = func
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| matches!(b.terminator(), Some(MInst::JumpTable { .. })).then_some(i))
        .collect();
    if dispatches.is_empty() {
        return;
    }
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();
    let mut predecessor = HashMap::default();
    let mut shared = HashSet::default();
    for block in &func.blocks {
        for target in block.successors() {
            if predecessor
                .insert(target, block.id)
                .is_some_and(|prev| prev != block.id)
            {
                shared.insert(target);
            }
        }
    }
    let mut dead = HashSet::default();
    for dispatch in dispatches {
        if let Some(plan) = plan(func, dispatch, &positions, &shared) {
            plan.apply(func, &mut dead);
        }
    }
    func.blocks.retain(|b| !dead.contains(&b.id));
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests;
