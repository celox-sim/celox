//! Schedule write-through RAM outputs with their deferred write ports.
use super::*;
use celox_design::BinaryOp;

pub(super) struct Readback {
    pub(super) root: usize,
    read: NodeId,
    enable: NodeId,
    remaining_guard: Option<NodeId>,
}

/// Move a staged output of the form `enable ? data : ram[index]` to a port
/// that writes the same data under that enable. An additional port predicate
/// (usually the bounds check) controls the memory write alone.
pub(super) fn schedule<A: Copy + Eq + Hash, B: Copy + Eq + Hash>(
    form: &NextStateForm<A>,
    order: &mut Vec<Write>,
    ports: &[[NodeId; 3]],
    arena: &SLTNodeArena<B>,
    address: &impl Fn(A, u32) -> B,
    captured: &HashSet<A>,
) -> HashMap<usize, Readback> {
    let mut seen = HashSet::default();
    let repeated: HashSet<_> = form
        .roots
        .iter()
        .map(|r| address(r.target, WORKING_REGION))
        .filter(|target| !seen.insert(*target))
        .collect();
    let mut outputs = HashMap::default();
    for w in order.iter() {
        let root = &form.roots[w.index];
        if captured.contains(&root.target)
            || repeated.contains(&address(root.target, WORKING_REGION))
            || arena.node_width(w.value) != Some(root.access.msb - root.access.lsb + 1)
        {
            continue;
        }
        let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = *arena.get(w.value)
        else {
            continue;
        };
        // A checked read supplies X on an invalid index. Retain the entire
        // read expression on the disabled edge, including this fallback.
        let input = match *arena.get(else_expr) {
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } if matches!(arena.get(else_expr), SLTNode::Constant(..)) => then_expr,
            _ => else_expr,
        };
        if let SLTNode::Input {
            variable, index, ..
        } = arena.get(input)
            && index
                .iter()
                .any(|i| matches!(i.kind, crate::SLTIndexKind::Unpacked { .. }))
        {
            outputs.insert((*variable, then_expr, cond), (w.index, else_expr));
        }
    }
    let mut readbacks = HashMap::default();
    let mut moved = HashSet::default();
    for (i, (port, &[_, data, guard])) in form.ports.iter().zip(ports).enumerate() {
        if captured.contains(&port.array) {
            continue;
        }
        let mut take = |enable, remaining_guard| {
            let (root, read) =
                outputs.remove(&(address(port.array, STABLE_REGION), data, enable))?;
            Some(Readback {
                root,
                read,
                enable,
                remaining_guard,
            })
        };
        let found = take(guard, None).or_else(|| {
            let SLTNode::Binary(a, BinaryOp::LogicAnd, b) = *arena.get(guard) else {
                return None;
            };
            take(a, Some(b)).or_else(|| take(b, Some(a)))
        });
        if let Some(readback) = found {
            moved.insert(readback.root);
            readbacks.insert(i, readback);
        }
    }
    order.retain(|w| !moved.contains(&w.index));
    readbacks
}

impl Readback {
    pub(super) fn emit<A, B: Copy + Eq + Hash + Debug + Display>(
        &self,
        b: &mut SIRBuilder<B>,
        arena: &SLTNodeArena<B>,
        lowerer: &mut SLTToSIRLowerer,
        lowered: &mut HashMap<NodeId, RegisterId>,
        root: &ValueRoot<A>,
        output: B,
        data: NodeId,
        store: impl FnOnce(RegisterId) -> SIRInstruction<B>,
    ) -> Result<(), String> {
        // Keep calculations with other users available to the rest of the
        // transition. A plain input slice can load directly on the write edge.
        let eager_data =
            (!matches!(arena.get(data), SLTNode::Input { index, .. } if index.is_empty()))
                .then(|| lowerer.lower(b, data, arena, lowered));
        let enable = lowerer.lower(b, self.enable, arena, lowered);
        let validity = self
            .remaining_guard
            .map(|g| lowerer.lower(b, g, arena, lowered));
        let width = root.access.msb - root.access.lsb + 1;
        let result = b.alloc_logic(width);
        let join = b.new_block_with(vec![result]);
        let write = b.new_block();
        let read = b.new_block();
        b.seal_block(SIRTerminator::Branch {
            cond: enable,
            true_block: (write, vec![]),
            false_block: (read, vec![]),
        });
        b.switch_to_block(read);
        let loaded = lower_arm(b, lowerer, arena, lowered, self.read);
        let loaded = lowerer.cast_reg_width(b, loaded, width);
        b.seal_block(SIRTerminator::Jump(join, vec![loaded]));
        b.switch_to_block(write);
        let value = eager_data.unwrap_or_else(|| lower_arm(b, lowerer, arena, lowered, data));
        let payload = lowerer.cast_reg_width(b, value, width);
        if let Some(cond) = validity {
            let valid = b.new_block();
            b.seal_block(SIRTerminator::Branch {
                cond,
                true_block: (valid, vec![]),
                false_block: (join, vec![payload]),
            });
            b.switch_to_block(valid);
        }
        b.emit(store(value));
        b.seal_block(SIRTerminator::Jump(join, vec![payload]));
        b.switch_to_block(join);
        b.emit(root_store(b, root, output, result)?);
        Ok(())
    }
}

// Registers defined inside one arm cannot be reused on a sibling edge or
// after the join. Keep cached operands that already dominated the decision.
fn lower_arm<B: Copy + Eq + Hash + Debug + Display>(
    b: &mut SIRBuilder<B>,
    lowerer: &mut SLTToSIRLowerer,
    arena: &SLTNodeArena<B>,
    lowered: &mut HashMap<NodeId, RegisterId>,
    node: NodeId,
) -> RegisterId {
    let value = lowerer.lower(b, node, arena, lowered);
    for local in lowerer.take_scheduled_region_insertions() {
        lowered.remove(&local);
    }
    value
}
