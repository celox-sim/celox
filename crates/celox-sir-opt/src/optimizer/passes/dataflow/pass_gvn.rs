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
        let cfg = GvnCfg::new(eu);
        let register_types = eu.register_map.clone();
        let mut state = GvnState::default();

        for &root in &cfg.roots {
            gvn_dom_dfs(
                root,
                true,
                eu,
                &cfg,
                &register_types,
                &load_versions,
                options.four_state,
                &mut state,
            );
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
        self.canonical_changes
            .push((register, self.canonical.get(&register).copied()));
        self.canonical.insert(register, canonical);
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
    match instruction {
        SIRInstruction::Store(_, _, _, _, triggers, capture_sites) => {
            !triggers.is_empty() || !capture_sites.is_empty()
        }
        SIRInstruction::Commit(_, _, _, _, triggers) => !triggers.is_empty(),
        SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => true,
        _ => false,
    }
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
        SIRInstruction::Load(..)
        | SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
    }
}

fn gvn_dom_dfs(
    root: usize,
    reset_root_loads: bool,
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &GvnCfg,
    register_types: &HashMap<RegisterId, RegisterType>,
    load_versions: &HashMap<RegisterId, StructuralLoadVersion>,
    four_state: bool,
    state: &mut GvnState,
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
                    eu,
                    cfg,
                    register_types,
                    load_versions,
                    four_state,
                    state,
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
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &GvnCfg,
    register_types: &HashMap<RegisterId, RegisterType>,
    load_versions: &HashMap<RegisterId, StructuralLoadVersion>,
    four_state: bool,
    state: &mut GvnState,
) {
    let block_id = cfg.block_ids[node];
    let mut removed_defs = Vec::new();
    if let Some(block) = eu.blocks.get_mut(&block_id) {
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

            if matches!(
                inst,
                SIRInstruction::Store(..)
                    | SIRInstruction::Commit(..)
                    | SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
            ) {
                state.bump_memory_epoch();
                if is_observable_barrier(inst) {
                    state.bump_observable_epoch();
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
    for register in removed_defs {
        eu.register_map.remove(&register);
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

fn apply_alias_to_offset(offset: &mut SIROffset, aliases: &HashMap<RegisterId, RegisterId>) {
    match offset {
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
        SIROffset::Dynamic(register) => {
            *register = resolve_canonical(*register, aliases);
        }
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            *index = resolve_canonical(*index, aliases);
            if let Some(dynamic_bit_offset) = dynamic_bit_offset {
                *dynamic_bit_offset = resolve_canonical(*dynamic_bit_offset, aliases);
            }
        }
    }
}

fn apply_aliases(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            *lhs = resolve_canonical(*lhs, aliases);
            *rhs = resolve_canonical(*rhs, aliases);
        }
        SIRInstruction::Unary(_, _, src) => {
            *src = resolve_canonical(*src, aliases);
        }
        SIRInstruction::Load(_, _, offset, _) => apply_alias_to_offset(offset, aliases),
        SIRInstruction::Store(_, offset, _, src, _, _) => {
            apply_alias_to_offset(offset, aliases);
            *src = resolve_canonical(*src, aliases);
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            apply_alias_to_offset(offset, aliases);
        }
        SIRInstruction::Concat(_, args) => {
            for arg in args {
                *arg = resolve_canonical(*arg, aliases);
            }
        }
        SIRInstruction::Slice(_, src, _, _) => {
            *src = resolve_canonical(*src, aliases);
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            *cond = resolve_canonical(*cond, aliases);
            *then_val = resolve_canonical(*then_val, aliases);
            *else_val = resolve_canonical(*else_val, aliases);
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for arg in args {
                *arg = resolve_canonical(*arg, aliases);
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            *old = resolve_canonical(*old, aliases);
            *new = resolve_canonical(*new, aliases);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::InstanceId;
    use celox_design::StateObjectId as VarId;
    use num_bigint::BigUint;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn loop_unit(
        entry_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        entry_args: Vec<RegisterId>,
        body_params: Vec<RegisterId>,
        body_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        loop_cond: RegisterId,
        backedge_args: Vec<RegisterId>,
        register_types: Vec<(RegisterId, RegisterType)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let entry_id = BlockId(0);
        let body_id = BlockId(1);
        let exit_id = BlockId(2);
        let unit = ExecutionUnit {
            entry_block_id: entry_id,
            blocks: [
                BasicBlock {
                    id: entry_id,
                    params: Vec::new(),
                    instructions: entry_instructions,
                    terminator: SIRTerminator::Jump(body_id, entry_args),
                },
                BasicBlock {
                    id: body_id,
                    params: body_params,
                    instructions: body_instructions,
                    terminator: SIRTerminator::Branch {
                        cond: loop_cond,
                        true_block: (body_id, backedge_args),
                        false_block: (exit_id, Vec::new()),
                    },
                },
                BasicBlock {
                    id: exit_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: register_types.into_iter().collect(),
        };
        unit.verify_result().unwrap();
        unit
    }

    #[test]
    fn removes_redundant_definition_after_rewriting_uses() {
        let r0 = RegisterId(0);
        let r1 = RegisterId(1);
        let r2 = RegisterId(2);
        let block_id = BlockId(0);
        let mut unit = ExecutionUnit {
            entry_block_id: block_id,
            blocks: std::iter::once((
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(r0, SIRValue::new(1u8)),
                        SIRInstruction::Imm(r1, SIRValue::new(1u8)),
                        SIRInstruction::Binary(r2, r1, BinaryOp::Add, r1),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ))
            .collect(),
            register_map: [
                (
                    r0,
                    RegisterType::Bit {
                        width: 8,
                        signed: false,
                    },
                ),
                (
                    r1,
                    RegisterType::Bit {
                        width: 8,
                        signed: false,
                    },
                ),
                (
                    r2,
                    RegisterType::Bit {
                        width: 8,
                        signed: false,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(
            unit.blocks[&block_id].instructions,
            vec![
                SIRInstruction::Imm(r0, SIRValue::new(1u8)),
                SIRInstruction::Binary(r2, r0, BinaryOp::Add, r0),
            ]
        );
        assert!(!unit.register_map.contains_key(&r1));
        unit.verify_result().unwrap();
    }

    #[test]
    fn reuses_cross_type_concat_bits_only_in_two_state_mode() {
        let r0 = RegisterId(0);
        let r1 = RegisterId(1);
        let packed_bit = RegisterId(2);
        let packed_logic = RegisterId(3);
        let block_id = BlockId(0);
        let fixture = ExecutionUnit {
            entry_block_id: block_id,
            blocks: std::iter::once((
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(r0, SIRValue::new(0u8)),
                        SIRInstruction::Imm(r1, SIRValue::new(1u8)),
                        SIRInstruction::Concat(packed_bit, vec![r1, r0]),
                        SIRInstruction::Store(
                            address(0),
                            SIROffset::Static(0),
                            2,
                            packed_bit,
                            Vec::new(),
                            Vec::new(),
                        ),
                        SIRInstruction::Concat(packed_logic, vec![r1, r0]),
                        SIRInstruction::Store(
                            address(1),
                            SIROffset::Static(0),
                            2,
                            packed_logic,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ))
            .collect(),
            register_map: [
                (r0, bit(1)),
                (r1, bit(1)),
                (packed_bit, bit(2)),
                (packed_logic, RegisterType::Logic { width: 2 }),
            ]
            .into_iter()
            .collect(),
        };

        let mut two_state = fixture.clone();
        GvnPass.run(&mut two_state, &PassOptions::default());
        assert!(matches!(
            two_state.blocks[&block_id].instructions[4],
            SIRInstruction::Unary(dst, UnaryOp::Ident, source)
                if dst == packed_logic && source == packed_bit
        ));
        two_state.verify_result().unwrap();

        let mut four_state = fixture;
        GvnPass.run(
            &mut four_state,
            &PassOptions {
                four_state: true,
                ..PassOptions::default()
            },
        );
        assert!(matches!(
            four_state.blocks[&block_id].instructions[4],
            SIRInstruction::Concat(dst, _)
                if dst == packed_logic
        ));
        four_state.verify_result().unwrap();
    }

    #[test]
    fn loop_block_cses_param_expressions_and_reuses_dominating_values() {
        let registers = (0..12)
            .map(|index| {
                (
                    RegisterId(index),
                    if index == 0 || index == 4 {
                        bit(1)
                    } else {
                        bit(8)
                    },
                )
            })
            .collect();
        let mut unit = loop_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(2u8)),
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(3u8)),
                SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Add, RegisterId(2)),
            ],
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![RegisterId(4), RegisterId(5), RegisterId(6)],
            vec![
                SIRInstruction::Binary(RegisterId(7), RegisterId(5), BinaryOp::Add, RegisterId(6)),
                SIRInstruction::Binary(RegisterId(8), RegisterId(5), BinaryOp::Add, RegisterId(6)),
                SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Xor, RegisterId(5)),
                // These operands are loop-invariant SSA values defined in the
                // preheader, so the dominating r3 definition is available in
                // every iteration. The block parameters above remain distinct.
                SIRInstruction::Binary(RegisterId(10), RegisterId(1), BinaryOp::Add, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(11), RegisterId(1), BinaryOp::Add, RegisterId(2)),
            ],
            RegisterId(4),
            vec![RegisterId(4), RegisterId(5), RegisterId(6)],
            registers,
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        let body = &unit.blocks[&BlockId(1)];
        assert!(
            body.instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(7), ..)) })
        );
        assert!(body.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(9), RegisterId(7), BinaryOp::Xor, RegisterId(5))
            )
        }));
        assert!(!unit.register_map.contains_key(&RegisterId(8)));
        assert!(!unit.register_map.contains_key(&RegisterId(10)));
        assert!(!unit.register_map.contains_key(&RegisterId(11)));
        assert!(unit.register_map.contains_key(&RegisterId(3)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn loop_block_cses_dynamic_loads_after_offset_aliasing() {
        let mut unit = loop_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(4u8)),
            ],
            vec![RegisterId(0), RegisterId(1)],
            vec![RegisterId(2), RegisterId(3)],
            vec![
                SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                SIRInstruction::Unary(RegisterId(5), UnaryOp::Ident, RegisterId(3)),
                SIRInstruction::Load(
                    RegisterId(6),
                    address(0),
                    SIROffset::Dynamic(RegisterId(4)),
                    8,
                ),
                SIRInstruction::Load(
                    RegisterId(7),
                    address(0),
                    SIROffset::Dynamic(RegisterId(5)),
                    8,
                ),
            ],
            RegisterId(2),
            vec![RegisterId(2), RegisterId(3)],
            vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(64)),
                (RegisterId(2), bit(1)),
                (RegisterId(3), bit(64)),
                (RegisterId(4), bit(64)),
                (RegisterId(5), bit(64)),
                (RegisterId(6), RegisterType::Logic { width: 8 }),
                (RegisterId(7), RegisterType::Logic { width: 8 }),
            ],
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        let body = &unit.blocks[&BlockId(1)];
        assert_eq!(
            body.instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            1
        );
        assert!(body.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Load(RegisterId(6), _, SIROffset::Dynamic(RegisterId(4)), 8)
            )
        }));
        assert!(!unit.register_map.contains_key(&RegisterId(5)));
        assert!(!unit.register_map.contains_key(&RegisterId(7)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn loop_store_is_a_load_cse_barrier_and_rewrites_dynamic_offset() {
        let addr = address(0);
        let mut unit = loop_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(4u8)),
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(9u8)),
            ],
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![RegisterId(3), RegisterId(4), RegisterId(5)],
            vec![
                SIRInstruction::Unary(RegisterId(6), UnaryOp::Ident, RegisterId(4)),
                SIRInstruction::Unary(RegisterId(7), UnaryOp::Ident, RegisterId(4)),
                SIRInstruction::Load(RegisterId(8), addr, SIROffset::Dynamic(RegisterId(6)), 8),
                SIRInstruction::Store(
                    addr,
                    SIROffset::Dynamic(RegisterId(7)),
                    8,
                    RegisterId(5),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(9), addr, SIROffset::Dynamic(RegisterId(7)), 8),
            ],
            RegisterId(3),
            vec![RegisterId(3), RegisterId(4), RegisterId(5)],
            vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(64)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(1)),
                (RegisterId(4), bit(64)),
                (RegisterId(5), bit(8)),
                (RegisterId(6), bit(64)),
                (RegisterId(7), bit(64)),
                (RegisterId(8), RegisterType::Logic { width: 8 }),
                (RegisterId(9), RegisterType::Logic { width: 8 }),
            ],
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        let body = &unit.blocks[&BlockId(1)];
        assert_eq!(
            body.instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            2
        );
        assert!(body.instructions.iter().all(|inst| match inst {
            SIRInstruction::Load(_, _, SIROffset::Dynamic(offset), _)
            | SIRInstruction::Store(_, SIROffset::Dynamic(offset), ..) => {
                *offset == RegisterId(6)
            }
            _ => true,
        }));
        assert!(!unit.register_map.contains_key(&RegisterId(7)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn loop_load_key_separates_address_offset_width_and_result_type() {
        let mut unit = loop_unit(
            vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
            vec![RegisterId(0)],
            vec![RegisterId(1)],
            vec![
                SIRInstruction::Load(RegisterId(2), address(0), SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(3), address(1), SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(4), address(0), SIROffset::Static(0), 16),
                SIRInstruction::Load(RegisterId(5), address(0), SIROffset::Static(1), 8),
                SIRInstruction::Load(RegisterId(6), address(0), SIROffset::Static(0), 8),
            ],
            RegisterId(1),
            vec![RegisterId(1)],
            vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), RegisterType::Logic { width: 8 }),
                (RegisterId(3), RegisterType::Logic { width: 8 }),
                (RegisterId(4), RegisterType::Logic { width: 16 }),
                (RegisterId(5), RegisterType::Logic { width: 8 }),
                (RegisterId(6), bit(8)),
            ],
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(
            unit.blocks[&BlockId(1)]
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            5
        );
        for register in 2..=6 {
            assert!(unit.register_map.contains_key(&RegisterId(register)));
        }
        unit.verify_result().unwrap();
    }

    #[test]
    fn loop_pure_key_separates_logic_bit_and_signed_bit_results() {
        let mut unit = loop_unit(
            vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
            vec![RegisterId(0)],
            vec![RegisterId(1)],
            vec![
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(5u8)),
                SIRInstruction::Imm(RegisterId(3), SIRValue::new(5u8)),
                SIRInstruction::Imm(RegisterId(4), SIRValue::new(5u8)),
                SIRInstruction::Imm(RegisterId(5), SIRValue::new(5u8)),
            ],
            RegisterId(1),
            vec![RegisterId(1)],
            vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), RegisterType::Logic { width: 8 }),
                (RegisterId(3), bit(8)),
                (
                    RegisterId(4),
                    RegisterType::Bit {
                        width: 8,
                        signed: true,
                    },
                ),
                (RegisterId(5), RegisterType::Logic { width: 8 }),
            ],
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        for register in 2..=4 {
            assert!(unit.register_map.contains_key(&RegisterId(register)));
        }
        assert!(!unit.register_map.contains_key(&RegisterId(5)));
        assert_eq!(unit.blocks[&BlockId(1)].instructions.len(), 3);
        unit.verify_result().unwrap();
    }

    #[test]
    fn loop_alias_rewrites_a_dominated_cross_block_use() {
        let entry_id = BlockId(0);
        let body_id = BlockId(1);
        let inner_id = BlockId(2);
        let exit_id = BlockId(3);
        let mut unit = ExecutionUnit {
            entry_block_id: entry_id,
            blocks: [
                BasicBlock {
                    id: entry_id,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                    ],
                    terminator: SIRTerminator::Jump(body_id, vec![RegisterId(0), RegisterId(1)]),
                },
                BasicBlock {
                    id: body_id,
                    params: vec![RegisterId(2), RegisterId(3)],
                    instructions: vec![
                        SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                        SIRInstruction::Unary(RegisterId(5), UnaryOp::Ident, RegisterId(3)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(2),
                        true_block: (inner_id, Vec::new()),
                        false_block: (exit_id, Vec::new()),
                    },
                },
                BasicBlock {
                    id: inner_id,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Binary(
                        RegisterId(6),
                        RegisterId(5),
                        BinaryOp::Add,
                        RegisterId(3),
                    )],
                    terminator: SIRTerminator::Jump(body_id, vec![RegisterId(2), RegisterId(3)]),
                },
                BasicBlock {
                    id: exit_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(1)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
                (RegisterId(5), bit(8)),
                (RegisterId(6), bit(8)),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert!(!unit.register_map.contains_key(&RegisterId(5)));
        assert_eq!(unit.blocks[&body_id].instructions.len(), 1);
        assert!(matches!(
            unit.blocks[&inner_id].instructions[0],
            SIRInstruction::Binary(_, RegisterId(4), BinaryOp::Add, RegisterId(3))
        ));
        unit.verify_result().unwrap();
    }

    #[test]
    fn immediate_key_keeps_payload_and_mask_structurally_separate() {
        // With the old `payload + [u64::MAX] + mask` encoding these two
        // distinct values both became [1, MAX, 2, MAX, 3].
        let limb = BigUint::from(1u8) << 64usize;
        let limb_2 = &limb << 64usize;
        let max = BigUint::from(u64::MAX);
        let value_a = SIRValue::new_four_state(
            BigUint::from(1u8),
            BigUint::from(2u8) + &max * &limb + BigUint::from(3u8) * &limb_2,
        );
        let value_b = SIRValue::new_four_state(
            BigUint::from(1u8) + &max * &limb + BigUint::from(2u8) * &limb_2,
            BigUint::from(3u8),
        );
        let block_id = BlockId(0);
        let mut unit = ExecutionUnit {
            entry_block_id: block_id,
            blocks: std::iter::once((
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), value_a),
                        SIRInstruction::Imm(RegisterId(1), value_b),
                        SIRInstruction::Binary(
                            RegisterId(2),
                            RegisterId(0),
                            BinaryOp::Xor,
                            RegisterId(1),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ))
            .collect(),
            register_map: (0..3)
                .map(|register| (RegisterId(register), RegisterType::Logic { width: 192 }))
                .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(unit.blocks[&block_id].instructions.len(), 3);
        assert!(unit.register_map.contains_key(&RegisterId(0)));
        assert!(unit.register_map.contains_key(&RegisterId(1)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn cfg_marks_exact_scc_members_instead_of_an_rpo_interval() {
        let entry = BlockId(0);
        let header = BlockId(1);
        let body = BlockId(2);
        let exit = BlockId(3);
        let unit = ExecutionUnit {
            entry_block_id: entry,
            blocks: [
                BasicBlock {
                    id: entry,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                    terminator: SIRTerminator::Jump(header, vec![RegisterId(0)]),
                },
                BasicBlock {
                    id: header,
                    params: vec![RegisterId(1)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (body, Vec::new()),
                        false_block: (exit, Vec::new()),
                    },
                },
                BasicBlock {
                    id: body,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(header, vec![RegisterId(1)]),
                },
                BasicBlock {
                    id: exit,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: [(RegisterId(0), bit(1)), (RegisterId(1), bit(1))]
                .into_iter()
                .collect(),
        };
        unit.verify_result().unwrap();

        let cfg = GvnCfg::new(&unit);
        let is_cyclic = |block| {
            let index = cfg.block_ids.iter().position(|id| *id == block).unwrap();
            cfg.cyclic[index]
        };
        assert!(!is_cyclic(entry));
        assert!(is_cyclic(header));
        assert!(is_cyclic(body));
        assert!(!is_cyclic(exit));
    }

    #[test]
    fn cfg_marks_every_member_of_an_irreducible_multi_entry_scc() {
        let entry = BlockId(0);
        let a = BlockId(1);
        let b = BlockId(2);
        let c = BlockId(3);
        let d = BlockId(4);
        let exit = BlockId(5);
        let unit = ExecutionUnit {
            entry_block_id: entry,
            blocks: [
                BasicBlock {
                    id: entry,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (a, Vec::new()),
                        false_block: (b, Vec::new()),
                    },
                },
                BasicBlock {
                    id: a,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(c, Vec::new()),
                },
                BasicBlock {
                    id: b,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(c, Vec::new()),
                },
                BasicBlock {
                    id: c,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (a, Vec::new()),
                        false_block: (d, Vec::new()),
                    },
                },
                BasicBlock {
                    id: d,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (b, Vec::new()),
                        false_block: (exit, Vec::new()),
                    },
                },
                BasicBlock {
                    id: exit,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: [(RegisterId(0), bit(1))].into_iter().collect(),
        };
        unit.verify_result().unwrap();

        let cfg = GvnCfg::new(&unit);
        let is_cyclic = |block| {
            let index = cfg.block_ids.iter().position(|id| *id == block).unwrap();
            cfg.cyclic[index]
        };
        assert!(!is_cyclic(entry));
        for block in [a, b, c, d] {
            assert!(
                is_cyclic(block),
                "b{} must be in the irreducible SCC",
                block.0
            );
        }
        assert!(!is_cyclic(exit));
    }

    #[test]
    fn load_availability_crosses_a_single_acyclic_edge() {
        let entry = BlockId(0);
        let next = BlockId(1);
        let addr = address(0);
        let mut unit = ExecutionUnit {
            entry_block_id: entry,
            blocks: [
                BasicBlock {
                    id: entry,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(0),
                        addr,
                        SIROffset::Static(0),
                        8,
                    )],
                    terminator: SIRTerminator::Jump(next, Vec::new()),
                },
                BasicBlock {
                    id: next,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(1),
                        addr,
                        SIROffset::Static(0),
                        8,
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: [
                (RegisterId(0), RegisterType::Logic { width: 8 }),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert!(unit.blocks[&next].instructions.is_empty());
        assert!(!unit.register_map.contains_key(&RegisterId(1)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn same_state_version_load_crosses_a_diamond_with_a_disjoint_store() {
        let entry = BlockId(0);
        let left = BlockId(1);
        let right = BlockId(2);
        let join = BlockId(3);
        let addr = address(0);
        let other_addr = address(1);
        let mut unit = ExecutionUnit {
            entry_block_id: entry,
            blocks: [
                BasicBlock {
                    id: entry,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Imm(RegisterId(3), SIRValue::new(7u8)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (left, Vec::new()),
                        false_block: (right, Vec::new()),
                    },
                },
                BasicBlock {
                    id: left,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        other_addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(join, Vec::new()),
                },
                BasicBlock {
                    id: right,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(join, Vec::new()),
                },
                BasicBlock {
                    id: join,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(2),
                        addr,
                        SIROffset::Static(0),
                        8,
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: [
                (RegisterId(0), bit(1)),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
                (RegisterId(2), RegisterType::Logic { width: 8 }),
                (RegisterId(3), RegisterType::Logic { width: 8 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(unit.blocks[&entry].instructions.len(), 3);
        assert!(unit.blocks[&join].instructions.is_empty());
        assert!(!unit.register_map.contains_key(&RegisterId(2)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn same_slot_store_on_one_arm_creates_a_join_version() {
        let entry = BlockId(0);
        let left = BlockId(1);
        let right = BlockId(2);
        let join = BlockId(3);
        let addr = address(0);
        let mut unit = ExecutionUnit {
            entry_block_id: entry,
            blocks: [
                BasicBlock {
                    id: entry,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(0), address(1), SIROffset::Static(0), 1),
                        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Imm(RegisterId(3), SIRValue::new(7u8)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (left, Vec::new()),
                        false_block: (right, Vec::new()),
                    },
                },
                BasicBlock {
                    id: left,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(join, Vec::new()),
                },
                BasicBlock {
                    id: right,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(join, Vec::new()),
                },
                BasicBlock {
                    id: join,
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(2),
                        addr,
                        SIROffset::Static(0),
                        8,
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: [
                (RegisterId(0), bit(1)),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
                (RegisterId(2), RegisterType::Logic { width: 8 }),
                (RegisterId(3), RegisterType::Logic { width: 8 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert!(matches!(
            unit.blocks[&join].instructions.as_slice(),
            [SIRInstruction::Load(RegisterId(2), ..)]
        ));
        assert!(unit.register_map.contains_key(&RegisterId(2)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn read_only_loop_reuses_a_dominating_preheader_load() {
        let addr = address(0);
        let mut unit = loop_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
            ],
            vec![RegisterId(0)],
            vec![RegisterId(2)],
            vec![SIRInstruction::Load(
                RegisterId(3),
                addr,
                SIROffset::Static(0),
                8,
            )],
            RegisterId(2),
            vec![RegisterId(2)],
            vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
                (RegisterId(2), bit(1)),
                (RegisterId(3), RegisterType::Logic { width: 8 }),
            ],
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            1
        );
        assert!(!unit.register_map.contains_key(&RegisterId(3)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn loop_carried_same_slot_store_creates_a_header_version() {
        let addr = address(0);
        let mut unit = loop_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                SIRInstruction::Imm(RegisterId(4), SIRValue::new(7u8)),
            ],
            vec![RegisterId(0)],
            vec![RegisterId(2)],
            vec![
                SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    8,
                    RegisterId(4),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            RegisterId(2),
            vec![RegisterId(2)],
            vec![
                (RegisterId(0), bit(1)),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
                (RegisterId(2), bit(1)),
                (RegisterId(3), RegisterType::Logic { width: 8 }),
                (RegisterId(4), RegisterType::Logic { width: 8 }),
            ],
        );

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            2
        );
        assert!(unit.register_map.contains_key(&RegisterId(3)));
        unit.verify_result().unwrap();
    }

    #[test]
    fn observable_callbacks_split_loads_but_a_disjoint_commit_does_not() {
        let block = BlockId(0);
        let addr = address(0);
        let other_addr = address(1);
        let mut unit = ExecutionUnit {
            entry_block_id: block,
            blocks: std::iter::once((
                block,
                BasicBlock {
                    id: block,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(2u8)),
                        SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Commit(
                            addr,
                            other_addr,
                            SIROffset::Static(0),
                            8,
                            Vec::new(),
                        ),
                        SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                        SIRInstruction::RuntimeEvent {
                            site_id: 1,
                            args: vec![RegisterId(3)],
                        },
                        SIRInstruction::Load(RegisterId(4), addr, SIROffset::Static(0), 8),
                        SIRInstruction::CombCaptureEvent {
                            site_id: 2,
                            args: vec![RegisterId(4)],
                            fatal_error_code: None,
                            consume_enabled: false,
                        },
                        SIRInstruction::Load(RegisterId(5), addr, SIROffset::Static(0), 8),
                        SIRInstruction::CombCaptureEnableIfChanged {
                            old: RegisterId(0),
                            new: RegisterId(1),
                            sites: vec![2],
                        },
                        SIRInstruction::Load(RegisterId(6), addr, SIROffset::Static(0), 8),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ))
            .collect(),
            register_map: [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), RegisterType::Logic { width: 8 }),
                (RegisterId(3), RegisterType::Logic { width: 8 }),
                (RegisterId(4), RegisterType::Logic { width: 8 }),
                (RegisterId(5), RegisterType::Logic { width: 8 }),
                (RegisterId(6), RegisterType::Logic { width: 8 }),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert_eq!(
            unit.blocks[&block]
                .instructions
                .iter()
                .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
                .count(),
            4
        );
        assert!(!unit.register_map.contains_key(&RegisterId(3)));
        for register in [2, 4, 5, 6] {
            assert!(unit.register_map.contains_key(&RegisterId(register)));
        }
        assert!(matches!(
            &unit.blocks[&block].instructions[4],
            SIRInstruction::RuntimeEvent { args, .. } if args == &vec![RegisterId(2)]
        ));
        unit.verify_result().unwrap();
    }

    #[test]
    fn dominator_availability_rolls_back_before_a_sibling() {
        let entry = BlockId(0);
        let left = BlockId(1);
        let right = BlockId(2);
        let mut unit = ExecutionUnit {
            entry_block_id: entry,
            blocks: [
                BasicBlock {
                    id: entry,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                        SIRInstruction::Imm(RegisterId(2), SIRValue::new(9u8)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (left, Vec::new()),
                        false_block: (right, Vec::new()),
                    },
                },
                BasicBlock {
                    id: left,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Binary(
                            RegisterId(3),
                            RegisterId(1),
                            BinaryOp::Add,
                            RegisterId(2),
                        ),
                        SIRInstruction::Store(
                            address(0),
                            SIROffset::Static(0),
                            8,
                            RegisterId(3),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
                BasicBlock {
                    id: right,
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Binary(
                            RegisterId(4),
                            RegisterId(1),
                            BinaryOp::Add,
                            RegisterId(2),
                        ),
                        SIRInstruction::Store(
                            address(1),
                            SIROffset::Static(0),
                            8,
                            RegisterId(4),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
            ]
            .into_iter()
            .collect(),
        };
        unit.verify_result().unwrap();

        GvnPass.run(&mut unit, &PassOptions::default());

        assert!(matches!(
            unit.blocks[&left].instructions[1],
            SIRInstruction::Store(_, _, _, RegisterId(3), ..)
        ));
        assert!(matches!(
            unit.blocks[&right].instructions[1],
            SIRInstruction::Store(_, _, _, RegisterId(4), ..)
        ));
        assert!(unit.register_map.contains_key(&RegisterId(3)));
        assert!(unit.register_map.contains_key(&RegisterId(4)));
        unit.verify_result().unwrap();
    }
}
