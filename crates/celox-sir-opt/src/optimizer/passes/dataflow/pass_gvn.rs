//! Global Value Numbering (GVN) pass for SIR.
//!
//! Assigns a canonical value number to each expression (opcode + operand
//! value numbers). If two instructions have the same value number, the
//! second is replaced with an alias to the first's result register.
//!
//! Pure expression availability is scoped by the dominator tree. Exact state
//! loads use structural StateSSA versions; dynamic or otherwise unversioned
//! loads retain the conservative path-local memory epoch.

use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use super::state_ssa::{MemoryVersionId, StateFragment, StateSsa};
use crate::PassOptions;
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};

pub(in crate::optimizer) struct GvnPass;

impl ExecutionUnitPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        let candidates = structural_load_candidates(eu);
        let load_versions = if candidates.is_empty() {
            HashMap::default()
        } else {
            SirCfg::analyze(eu)
                .ok()
                .map(|cfg| structural_load_versions(eu, &cfg, &candidates))
                .unwrap_or_default()
        };
        drop(candidates);
        let cfg = GvnCfg::new(eu);
        let mut state = GvnState::default();
        let mut removed_defs = Vec::new();

        for &root in &cfg.roots {
            gvn_dom_dfs(
                root,
                true,
                &mut eu.blocks,
                &cfg,
                &eu.register_map,
                &load_versions,
                options.four_state,
                &mut state,
                &mut removed_defs,
            );
        }
        // Keep types available throughout the traversal without cloning the
        // entire register table. Deletions are only observable after the pass.
        for register in removed_defs {
            eu.register_map.remove(&register);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct StructuralLoadVersion {
    fragment: StateFragment,
    version: MemoryVersionId,
}

fn structural_load_versions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    candidates: &HashMap<u32, HashSet<RegisterId>>,
) -> HashMap<RegisterId, StructuralLoadVersion> {
    let mut versions = HashMap::default();
    let mut regions = candidates.keys().copied().collect::<Vec<_>>();
    regions.sort_unstable();
    for region in regions {
        let eligible = &candidates[&region];
        let Ok(state) = StateSsa::analyze_selected_loads(eu, cfg, region, eligible) else {
            continue;
        };
        for &block_id in &cfg.block_ids {
            for (instruction_index, instruction) in
                eu.blocks[&block_id].instructions.iter().enumerate()
            {
                let SIRInstruction::Load(destination, address, _, _) = instruction else {
                    continue;
                };
                if address.region != region {
                    continue;
                }
                if !eligible.contains(destination) {
                    continue;
                }
                let Some((slot, version)) =
                    state.read_version(block_id, instruction_index, *destination)
                else {
                    continue;
                };
                versions.insert(
                    *destination,
                    StructuralLoadVersion {
                        fragment: state.slots[slot].fragment,
                        version,
                    },
                );
            }
        }
    }
    versions
}

fn structural_load_candidates(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<u32, HashSet<RegisterId>> {
    let mut shapes = HashMap::<(StateFragment, ResultTypeKey), Vec<RegisterId>>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            let SIRInstruction::Load(destination, address, SIROffset::Static(bit_offset), width) =
                instruction
            else {
                continue;
            };
            let Some(result_type) = result_type_key(*destination, &eu.register_map) else {
                continue;
            };
            let fragment = StateFragment::from_access(
                *address,
                *bit_offset,
                *width,
                &eu.register_map[destination],
            );
            shapes
                .entry((fragment, result_type))
                .or_default()
                .push(*destination);
        }
    }

    let mut candidates = HashMap::<u32, HashSet<RegisterId>>::default();
    for ((fragment, _), destinations) in shapes {
        if destinations.len() < 2 {
            continue;
        }
        candidates
            .entry(fragment.addr.region)
            .or_default()
            .extend(destinations);
    }
    candidates
}

struct GvnCfg {
    block_ids: Vec<BlockId>,
    predecessors: Vec<Vec<usize>>,
    dom_children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    cyclic: Vec<bool>,
}

impl GvnCfg {
    fn new(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Self {
        let block_ids = rpo_blocks(eu);
        let index: HashMap<BlockId, usize> = block_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(idx, block)| (block, idx))
            .collect();
        let mut successors = vec![Vec::new(); block_ids.len()];
        let mut preds = vec![Vec::new(); block_ids.len()];
        for (idx, block_id) in block_ids.iter().copied().enumerate() {
            let Some(block) = eu.blocks.get(&block_id) else {
                continue;
            };
            for succ in terminator_successors(&block.terminator) {
                if let Some(&succ_idx) = index.get(&succ) {
                    successors[idx].push(succ_idx);
                    preds[succ_idx].push(idx);
                }
            }
        }
        for edges in successors.iter_mut().chain(&mut preds) {
            edges.sort_unstable();
            edges.dedup();
        }

        // Kosaraju's second pass. `block_ids` is reverse postorder of the
        // forward graph, so traversing the predecessor graph in this order
        // yields exact strongly connected components in O(B + E).
        let mut component = vec![None; block_ids.len()];
        let mut components = Vec::<Vec<usize>>::new();
        for seed in 0..block_ids.len() {
            if component[seed].is_some() {
                continue;
            }
            let component_id = components.len();
            let mut members = Vec::new();
            let mut stack = vec![seed];
            component[seed] = Some(component_id);
            while let Some(node) = stack.pop() {
                members.push(node);
                for &pred in &preds[node] {
                    if component[pred].is_none() {
                        component[pred] = Some(component_id);
                        stack.push(pred);
                    }
                }
            }
            components.push(members);
        }
        let mut cyclic = vec![false; block_ids.len()];
        for members in components {
            let is_cycle = members.len() > 1
                || members
                    .first()
                    .is_some_and(|&node| successors[node].contains(&node));
            if is_cycle {
                for node in members {
                    cyclic[node] = true;
                }
            }
        }

        let mut idom: Vec<Option<usize>> = vec![None; block_ids.len()];
        let roots = block_ids
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(idx, block)| {
                (block == eu.entry_block_id || preds[idx].is_empty()).then_some(idx)
            })
            .collect::<Vec<_>>();
        let mut is_root = vec![false; block_ids.len()];
        for &root in &roots {
            idom[root] = Some(root);
            is_root[root] = true;
        }

        let mut changed = true;
        while changed {
            changed = false;
            for idx in 0..block_ids.len() {
                if is_root[idx] {
                    continue;
                }
                let mut defined_preds = preds[idx]
                    .iter()
                    .copied()
                    .filter(|pred| idom[*pred].is_some());
                let Some(mut new_idom) = defined_preds.next() else {
                    continue;
                };
                for pred in defined_preds {
                    new_idom = intersect_idom(pred, new_idom, &idom);
                }
                if idom[idx] != Some(new_idom) {
                    idom[idx] = Some(new_idom);
                    changed = true;
                }
            }
        }

        let mut dom_children = vec![Vec::new(); block_ids.len()];
        for (idx, parent) in idom.iter().copied().enumerate() {
            if let Some(parent) = parent
                && parent != idx
            {
                dom_children[parent].push(idx);
            }
        }
        for children in &mut dom_children {
            children.sort_unstable();
        }

        Self {
            block_ids,
            predecessors: preds,
            dom_children,
            roots,
            cyclic,
        }
    }
}

fn intersect_idom(mut a: usize, mut b: usize, idom: &[Option<usize>]) -> usize {
    while a != b {
        while a > b {
            a = idom[a].expect("idom must be known for intersect");
        }
        while b > a {
            b = idom[b].expect("idom must be known for intersect");
        }
    }
    a
}

fn rpo_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Vec<BlockId> {
    fn visit(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        start: BlockId,
        seen: &mut crate::HashSet<BlockId>,
        out: &mut Vec<BlockId>,
    ) {
        let mut stack = vec![(start, false)];
        while let Some((block_id, expanded)) = stack.pop() {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            if expanded {
                out.push(block_id);
                continue;
            }
            if !seen.insert(block_id) {
                continue;
            }
            stack.push((block_id, true));
            let mut succs = terminator_successors(&eu.blocks[&block_id].terminator);
            succs.sort_unstable();
            succs.reverse();
            for succ in succs {
                if !seen.contains(&succ) {
                    stack.push((succ, false));
                }
            }
        }
    }

    let mut seen = crate::HashSet::default();
    let mut postorder = Vec::new();
    visit(eu, eu.entry_block_id, &mut seen, &mut postorder);
    let mut rest = eu.blocks.keys().copied().collect::<Vec<_>>();
    rest.sort_unstable();
    for block_id in rest {
        if !seen.contains(&block_id) {
            visit(eu, block_id, &mut seen, &mut postorder);
        }
    }
    postorder.reverse();
    postorder
}

fn terminator_successors(term: &SIRTerminator) -> Vec<BlockId> {
    match term {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| case.target)
            .chain(std::iter::once(*default))
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ResultTypeKey {
    Logic { width: usize },
    Bit { width: usize, signed: bool },
}

fn result_type_key(
    register: RegisterId,
    register_types: &HashMap<RegisterId, RegisterType>,
) -> Option<ResultTypeKey> {
    match register_types.get(&register)? {
        RegisterType::Logic { width } => Some(ResultTypeKey::Logic { width: *width }),
        RegisterType::Bit { width, signed } => Some(ResultTypeKey::Bit {
            width: *width,
            signed: *signed,
        }),
    }
}

/// A structural pure-expression key. Payload and mask are separate fields;
/// concatenating them with a sentinel is not injective for arbitrary-width
/// values because the sentinel can occur in either digit vector.
#[derive(Clone, PartialEq, Eq, Hash)]
enum PureExprKey {
    Imm { payload: Vec<u64>, mask: Vec<u64> },
    Binary(BinaryOp, RegisterId, RegisterId),
    Unary(UnaryOp, RegisterId),
    Concat(Vec<RegisterId>),
    Slice(RegisterId, usize, usize),
    Mux(RegisterId, RegisterId, RegisterId),
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PureValueKey {
    expression: PureExprKey,
    result_type: ResultTypeKey,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct LoadKey {
    addr: RegionedAbsoluteAddr,
    offset: SIROffset,
    width: usize,
    result_type: ResultTypeKey,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct EpochLoadKey {
    epoch: u64,
    load: LoadKey,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct StructuralLoadKey {
    version: StructuralLoadVersion,
    result_type: ResultTypeKey,
    observable_epoch: u64,
}

#[derive(Clone, Copy)]
struct GvnCheckpoint {
    value_inserts: usize,
    canonical_changes: usize,
    constant_changes: usize,
    load_inserts: usize,
    structural_load_inserts: usize,
    concat_inserts: usize,
    memory_epoch: u64,
    observable_epoch: u64,
}

#[derive(Default)]
struct GvnState {
    values: HashMap<PureValueKey, RegisterId>,
    value_inserts: Vec<PureValueKey>,
    canonical: HashMap<RegisterId, RegisterId>,
    canonical_changes: Vec<(RegisterId, Option<RegisterId>)>,
    constants: HashMap<RegisterId, u64>,
    constant_changes: Vec<(RegisterId, Option<u64>)>,
    loads: HashMap<EpochLoadKey, RegisterId>,
    load_inserts: Vec<EpochLoadKey>,
    structural_loads: HashMap<StructuralLoadKey, RegisterId>,
    structural_load_inserts: Vec<StructuralLoadKey>,
    two_state_concats: HashMap<Vec<RegisterId>, RegisterId>,
    concat_inserts: Vec<Vec<RegisterId>>,
    memory_epoch: u64,
    next_memory_epoch: u64,
    observable_epoch: u64,
    next_observable_epoch: u64,
}

impl GvnState {
    fn checkpoint(&self) -> GvnCheckpoint {
        GvnCheckpoint {
            value_inserts: self.value_inserts.len(),
            canonical_changes: self.canonical_changes.len(),
            constant_changes: self.constant_changes.len(),
            load_inserts: self.load_inserts.len(),
            structural_load_inserts: self.structural_load_inserts.len(),
            concat_inserts: self.concat_inserts.len(),
            memory_epoch: self.memory_epoch,
            observable_epoch: self.observable_epoch,
        }
    }

    fn rollback(&mut self, checkpoint: GvnCheckpoint) {
        while self.value_inserts.len() > checkpoint.value_inserts {
            let key = self.value_inserts.pop().unwrap();
            self.values.remove(&key);
        }
        while self.canonical_changes.len() > checkpoint.canonical_changes {
            let (register, old) = self.canonical_changes.pop().unwrap();
            if let Some(old) = old {
                self.canonical.insert(register, old);
            } else {
                self.canonical.remove(&register);
            }
        }
        while self.constant_changes.len() > checkpoint.constant_changes {
            let (register, old) = self.constant_changes.pop().unwrap();
            if let Some(old) = old {
                self.constants.insert(register, old);
            } else {
                self.constants.remove(&register);
            }
        }
        while self.load_inserts.len() > checkpoint.load_inserts {
            let key = self.load_inserts.pop().unwrap();
            self.loads.remove(&key);
        }
        while self.structural_load_inserts.len() > checkpoint.structural_load_inserts {
            let key = self.structural_load_inserts.pop().unwrap();
            self.structural_loads.remove(&key);
        }
        while self.concat_inserts.len() > checkpoint.concat_inserts {
            let key = self.concat_inserts.pop().unwrap();
            self.two_state_concats.remove(&key);
        }
        self.memory_epoch = checkpoint.memory_epoch;
        self.observable_epoch = checkpoint.observable_epoch;
    }

    fn set_canonical(&mut self, register: RegisterId, canonical: RegisterId) {
        let old = self.canonical.get(&register).copied();
        let next = (register != canonical).then_some(canonical);
        if old == next {
            return;
        }
        self.canonical_changes.push((register, old));
        if let Some(next) = next {
            self.canonical.insert(register, next);
        } else {
            self.canonical.remove(&register);
        }
    }

    fn set_constant(&mut self, register: RegisterId, value: u64) {
        self.constant_changes
            .push((register, self.constants.get(&register).copied()));
        self.constants.insert(register, value);
    }

    fn insert_value(&mut self, key: PureValueKey, register: RegisterId) {
        debug_assert!(!self.values.contains_key(&key));
        self.values.insert(key.clone(), register);
        self.value_inserts.push(key);
    }

    fn bump_memory_epoch(&mut self) {
        self.next_memory_epoch = self
            .next_memory_epoch
            .checked_add(1)
            .expect("a compilation cannot contain u64::MAX memory barriers");
        self.memory_epoch = self.next_memory_epoch;
    }

    fn available_load(&self, load: &LoadKey) -> Option<RegisterId> {
        self.loads
            .get(&EpochLoadKey {
                epoch: self.memory_epoch,
                load: load.clone(),
            })
            .copied()
    }

    fn insert_load(&mut self, load: LoadKey, register: RegisterId) {
        let key = EpochLoadKey {
            epoch: self.memory_epoch,
            load,
        };
        debug_assert!(!self.loads.contains_key(&key));
        self.loads.insert(key.clone(), register);
        self.load_inserts.push(key);
    }

    fn structural_load_key(
        &self,
        version: StructuralLoadVersion,
        result_type: ResultTypeKey,
    ) -> StructuralLoadKey {
        StructuralLoadKey {
            version,
            result_type,
            observable_epoch: self.observable_epoch,
        }
    }

    fn available_structural_load(
        &self,
        version: StructuralLoadVersion,
        result_type: ResultTypeKey,
    ) -> Option<RegisterId> {
        self.structural_loads
            .get(&self.structural_load_key(version, result_type))
            .copied()
    }

    fn insert_structural_load(
        &mut self,
        version: StructuralLoadVersion,
        result_type: ResultTypeKey,
        register: RegisterId,
    ) {
        let key = self.structural_load_key(version, result_type);
        debug_assert!(!self.structural_loads.contains_key(&key));
        self.structural_loads.insert(key.clone(), register);
        self.structural_load_inserts.push(key);
    }

    fn bump_observable_epoch(&mut self) {
        self.next_observable_epoch = self
            .next_observable_epoch
            .checked_add(1)
            .expect("a compilation cannot contain u64::MAX observable barriers");
        self.observable_epoch = self.next_observable_epoch;
    }

    fn insert_two_state_concat(&mut self, arguments: Vec<RegisterId>, register: RegisterId) {
        if self.two_state_concats.contains_key(&arguments) {
            return;
        }
        self.two_state_concats.insert(arguments.clone(), register);
        self.concat_inserts.push(arguments);
    }
}

fn is_observable_barrier(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    instruction.is_observable()
}

fn resolve_canonical(
    mut register: RegisterId,
    canonical: &HashMap<RegisterId, RegisterId>,
) -> RegisterId {
    while let Some(&next) = canonical.get(&register) {
        if next == register {
            break;
        }
        register = next;
    }
    register
}

fn pure_expression_key(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<PureExprKey> {
    if !inst.is_pure() {
        return None;
    }
    match inst {
        SIRInstruction::Imm(_, value) => Some(PureExprKey::Imm {
            payload: value.payload.to_u64_digits(),
            mask: value.mask.to_u64_digits(),
        }),
        SIRInstruction::Binary(_, lhs, op, rhs) => {
            let (lhs, rhs) = if op.is_commutative() && lhs > rhs {
                (*rhs, *lhs)
            } else {
                (*lhs, *rhs)
            };
            Some(PureExprKey::Binary(*op, lhs, rhs))
        }
        SIRInstruction::Unary(_, op, src) => Some(PureExprKey::Unary(*op, *src)),
        SIRInstruction::Concat(_, args) => Some(PureExprKey::Concat(args.clone())),
        SIRInstruction::Slice(_, src, offset, width) => {
            Some(PureExprKey::Slice(*src, *offset, *width))
        }
        SIRInstruction::Mux(_, cond, then_value, else_value) => {
            Some(PureExprKey::Mux(*cond, *then_value, *else_value))
        }
        _ => None,
    }
}

fn gvn_dom_dfs(
    root: usize,
    reset_root_loads: bool,
    blocks: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
    cfg: &GvnCfg,
    register_types: &HashMap<RegisterId, RegisterType>,
    load_versions: &HashMap<RegisterId, StructuralLoadVersion>,
    four_state: bool,
    state: &mut GvnState,
    removed_defs: &mut Vec<RegisterId>,
) {
    enum Work {
        Enter { node: usize, reset_loads: bool },
        Exit(GvnCheckpoint),
    }

    let mut work = vec![Work::Enter {
        node: root,
        reset_loads: reset_root_loads,
    }];
    while let Some(item) = work.pop() {
        match item {
            Work::Exit(checkpoint) => state.rollback(checkpoint),
            Work::Enter { node, reset_loads } => {
                let checkpoint = state.checkpoint();
                if reset_loads {
                    state.bump_memory_epoch();
                }
                process_gvn_block(
                    node,
                    blocks,
                    cfg,
                    register_types,
                    load_versions,
                    four_state,
                    state,
                    removed_defs,
                );

                work.push(Work::Exit(checkpoint));
                for &child in cfg.dom_children[node].iter().rev() {
                    // Load availability crosses only an unambiguous acyclic
                    // CFG edge. Pure availability follows dominance without
                    // regard to loops or memory effects.
                    let inherit_loads = !cfg.cyclic[child]
                        && cfg.predecessors[child].len() == 1
                        && cfg.predecessors[child][0] == node;
                    work.push(Work::Enter {
                        node: child,
                        reset_loads: !inherit_loads,
                    });
                }
            }
        }
    }
}

fn process_gvn_block(
    node: usize,
    blocks: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
    cfg: &GvnCfg,
    register_types: &HashMap<RegisterId, RegisterType>,
    load_versions: &HashMap<RegisterId, StructuralLoadVersion>,
    four_state: bool,
    state: &mut GvnState,
    removed_defs: &mut Vec<RegisterId>,
) {
    let block_id = cfg.block_ids[node];
    if let Some(block) = blocks.get_mut(&block_id) {
        // A block parameter is a phi-like SSA definition and therefore an
        // independent leaf value number. It is deliberately not equated with
        // any incoming edge argument, including a loop backedge argument.
        for &parameter in &block.params {
            state.set_canonical(parameter, parameter);
        }

        let mut redundant = crate::HashSet::default();
        for inst in &mut block.instructions {
            apply_aliases(inst, &state.canonical);

            if let SIRInstruction::Load(dst, addr, offset, width) = inst {
                let Some(result_type) = result_type_key(*dst, register_types) else {
                    state.set_canonical(*dst, *dst);
                    continue;
                };
                if let Some(&version) = load_versions.get(dst) {
                    if let Some(existing) = state.available_structural_load(version, result_type) {
                        state.set_canonical(*dst, existing);
                        redundant.insert(*dst);
                    } else {
                        state.insert_structural_load(version, result_type, *dst);
                        state.set_canonical(*dst, *dst);
                    }
                    continue;
                }
                let key = LoadKey {
                    addr: *addr,
                    offset: offset.clone(),
                    width: *width,
                    result_type,
                };
                if let Some(existing) = state.available_load(&key) {
                    state.set_canonical(*dst, existing);
                    redundant.insert(*dst);
                } else {
                    state.insert_load(key, *dst);
                    state.set_canonical(*dst, *dst);
                }
                continue;
            }

            if inst.has_side_effects() {
                state.bump_memory_epoch();
                if is_observable_barrier(inst) {
                    state.bump_observable_epoch();
                }
                if let Some(dst) = def_reg(inst) {
                    state.set_canonical(dst, dst);
                }
                continue;
            }

            let Some(dst) = def_reg(inst) else {
                continue;
            };
            let Some(result_type) = result_type_key(dst, register_types) else {
                state.set_canonical(dst, dst);
                continue;
            };

            if let SIRInstruction::Mux(_, cond, then_value, else_value) = inst
                && let Some(condition) = state.constants.get(cond).copied()
            {
                let selected = if condition != 0 {
                    *then_value
                } else {
                    *else_value
                };
                if result_type_key(selected, register_types) == Some(result_type) {
                    state.set_canonical(dst, selected);
                    redundant.insert(dst);
                    continue;
                }
            }

            let expression = pure_expression_key(inst)
                .expect("every remaining register definition is a pure expression");
            let key = PureValueKey {
                expression,
                result_type,
            };
            if let Some(&existing) = state.values.get(&key) {
                state.set_canonical(dst, existing);
                redundant.insert(dst);
            } else if !four_state
                && let PureExprKey::Concat(arguments) = &key.expression
                && let Some(&existing) = state.two_state_concats.get(arguments)
                && register_types
                    .get(&existing)
                    .is_some_and(|ty| ty.width() == register_types[&dst].width())
            {
                // A Bit and a Logic Concat with the same operands have the
                // same raw value in two-state mode. Keep the destination
                // definition and type, but avoid rebuilding the lanes.
                *inst = SIRInstruction::Unary(dst, UnaryOp::Ident, existing);
                state.insert_value(key, dst);
                state.set_canonical(dst, dst);
            } else {
                if let PureExprKey::Concat(arguments) = &key.expression
                    && !four_state
                {
                    state.insert_two_state_concat(arguments.clone(), dst);
                }
                state.insert_value(key, dst);
                state.set_canonical(dst, dst);
                if let SIRInstruction::Imm(_, value) = inst
                    && let Some(value) = super::shared::sir_value_to_u64(value)
                {
                    state.set_constant(dst, value);
                }
            }
        }

        apply_aliases_to_terminator(&mut block.terminator, &state.canonical);
        if !redundant.is_empty() {
            block.instructions.retain(|inst| {
                let remove = def_reg(inst).is_some_and(|dst| redundant.contains(&dst));
                if remove && let Some(dst) = def_reg(inst) {
                    removed_defs.push(dst);
                }
                !remove
            });
        }
    }
}

fn apply_aliases_to_terminator(
    term: &mut SIRTerminator,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    match term {
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            *cond = resolve_canonical(*cond, aliases);
            for arg in &mut true_block.1 {
                *arg = resolve_canonical(*arg, aliases);
            }
            for arg in &mut false_block.1 {
                *arg = resolve_canonical(*arg, aliases);
            }
        }
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                *arg = resolve_canonical(*arg, aliases);
            }
        }
        SIRTerminator::Switch { selector, .. } => {
            *selector = resolve_canonical(*selector, aliases);
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn apply_aliases(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    inst.for_each_use_mut(|register| {
        if let Some(&alias) = aliases.get(register) {
            *register = alias;
        }
    });
}

#[cfg(test)]
mod tests;
