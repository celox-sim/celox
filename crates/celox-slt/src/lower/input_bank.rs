//! Lower a scalar register bank to a multi-way selection, so each load
//! executes only on the edge which needs its value.
use super::*;
use celox_sir::SIRSwitchCase;

pub(super) struct InputBank {
    pub(super) selector: NodeId,
    arms: Vec<NodeId>,
    targets: Vec<usize>,
    default: usize,
}

fn selector_bit<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    condition: NodeId,
) -> Option<(NodeId, usize)> {
    match *arena.get(condition) {
        SLTNode::Slice { expr, access } if access.msb == access.lsb => Some((expr, access.lsb)),
        _ => None,
    }
}

// Lower literals with their original conversions, including nested concatenations of X/Z.
fn literal_arm<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, node: NodeId) -> bool {
    let mut pending = vec![node];
    let mut seen = crate::HashSet::default();
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Constant(..) => {}
            SLTNode::Concat(parts) => pending.extend(parts.iter().map(|&(value, _)| value)),
            SLTNode::Slice { expr, .. } | SLTNode::Unary(_, expr) => pending.push(*expr),
            _ => return false,
        }
    }
    true
}

impl InputBank {
    pub(super) fn recognize<A: Clone + Eq + Hash>(
        arena: &SLTNodeArena<A>,
        root: NodeId,
        cache: &crate::HashMap<NodeId, RegisterId>,
        slice_cache: &crate::HashMap<(NodeId, BitAccess), RegisterId>,
    ) -> Option<Self> {
        let SLTNode::Mux { cond, .. } = *arena.get(root) else {
            return None;
        };
        let (selector, high) = selector_bit(arena, cond)?;
        let width = arena.node_width(selector)?;
        if !(3..=8).contains(&width) || high != width - 1 {
            return None;
        }
        let result_width = arena.node_width(root)?;
        let mut targets = Vec::with_capacity(1 << width);
        let mut arms = Vec::new();
        let mut arm_indices = crate::HashMap::default();
        let mut counts = Vec::new();
        let mut input_count = 0;
        let mut input_entries = 0;
        // A test can be absent when both bit values lead to the same subtree.
        // Follow at most eight tests per address, including across such holes.
        for address in 0..1usize << width {
            let mut node = root;
            let mut upper_bit = width;
            while let SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } = *arena.get(node)
            {
                let (source, bit) = selector_bit(arena, cond)?;
                if source != selector
                    || bit >= upper_bit
                    || cache.contains_key(&node)
                    || cache.contains_key(&cond)
                    || slice_cache.contains_key(&(source, BitAccess::new(bit, bit)))
                {
                    return None;
                }
                upper_bit = bit;
                node = if address & (1 << bit) == 0 {
                    else_expr
                } else {
                    then_expr
                };
            }
            if arena.node_width(node)? != result_width {
                return None;
            }
            let is_input =
                matches!(arena.get(node), SLTNode::Input { index, .. } if index.is_empty());
            input_entries += usize::from(is_input);
            let arm = if let Some(&arm) = arm_indices.get(&node) {
                arm
            } else {
                if !is_input && !literal_arm(arena, node) {
                    return None;
                }
                input_count += usize::from(is_input);
                let arm = arms.len();
                arms.push(node);
                counts.push(0usize);
                arm_indices.insert(node, arm);
                arm
            };
            targets.push(arm);
            counts[arm] += 1;
        }
        if input_count < 8
            || input_entries < targets.len() / 2
            || !crate::decode::known_selector(arena, selector)
        {
            return None;
        }
        let default = (0..arms.len()).max_by_key(|&i| counts[i])?;
        Some(Self {
            selector,
            arms,
            targets,
            default,
        })
    }

    pub(super) fn emit<A: Hash + Eq + Clone + std::fmt::Debug + std::fmt::Display>(
        self,
        lowerer: &mut SLTToSIRLowerer,
        builder: &mut SIRBuilder<A>,
        selector: RegisterId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> RegisterId {
        let blocks: Vec<_> = self.arms.iter().map(|_| builder.new_block()).collect();
        let cases = self
            .targets
            .iter()
            .enumerate()
            .filter(|(_, arm)| **arm != self.default)
            .map(|(key, arm)| SIRSwitchCase {
                value: key.into(),
                target: blocks[*arm],
            })
            .collect();
        let result = builder.alloc_logic(crate::get_width(self.arms[0], arena));
        let merge = builder.new_block_with(vec![result]);
        builder.seal_block(SIRTerminator::Switch {
            selector,
            cases,
            default: blocks[self.default],
        });
        for (block, arm) in blocks.into_iter().zip(self.arms) {
            builder.switch_to_block(block);
            let transaction = lowerer.cache_transaction();
            let value = lowerer.lower_inner(builder, arm, arena, cache, None, allow_cache);
            builder.seal_block(SIRTerminator::Jump(merge, vec![value]));
            if allow_cache {
                lowerer.rollback_cache(cache, transaction);
            }
        }
        builder.switch_to_block(merge);
        result
    }
}
