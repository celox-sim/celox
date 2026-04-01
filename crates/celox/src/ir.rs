use crate::{
    HashMap,
    logic_tree::{LogicPath, SLTNodeArena, SymbolicStore},
};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::{collections::BTreeSet, fmt::Display};
use veryl_analyzer::ir::{VarId, VarPath, Variable};

/// Error returned by [`Program::get_addr`] when a path-based variable lookup fails.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AddrLookupError {
    #[error("Instance not found: {path}")]
    InstanceNotFound { path: String },
    #[error("Variable not found: {path}")]
    VariableNotFound { path: String },
    #[error("Ambiguous variable path: {path} — multiple variables share this path")]
    AmbiguousPath { path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DomainKind {
    ClockPosedge,
    ClockNegedge,
    ResetAsyncHigh,
    ResetAsyncLow,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortTypeKind {
    Clock,
    ResetAsyncHigh,
    ResetAsyncLow,
    ResetSyncHigh,
    ResetSyncLow,
    Logic,
    Bit,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TriggerIdWithKind {
    pub kind: DomainKind,
    pub id: usize,
}

#[derive(Clone)]
pub struct VariableInfo {
    pub width: usize,
    pub id: VarId,
    pub path: VarPath,
    pub is_4state: bool,
    pub kind: DomainKind,
    pub var_kind: veryl_analyzer::ir::VarKind,
    pub type_kind: PortTypeKind,
    /// Per-dimension sizes for array ports (e.g. `[4]` for `logic<32>[4]`).
    /// Empty means scalar.
    pub array_dims: Vec<usize>,
}

impl fmt::Debug for VariableInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VariableInfo")
            .field("width", &self.width)
            .field("id", &self.id)
            .field("is_4state", &self.is_4state)
            .field("kind", &self.kind)
            .field("type_kind", &self.type_kind)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TriggerSet<A> {
    pub clock: A,
    pub resets: Vec<A>,
}
/// Compilation plan for eval_comb when the CLIF instruction count exceeds
/// Cranelift's limit. Mutually exclusive strategies.
#[derive(Debug, Clone)]
pub enum EvalCombPlan {
    /// EU-boundary / single-block splitting with live regs in tail-call args.
    TailCallChunks(Vec<crate::optimizer::coalescing::TailCallChunk>),
    /// Memory-spilled multi-block splitting with scratch memory.
    MemorySpilled(crate::optimizer::coalescing::pass_tail_call_split::MemorySpilledPlan),
}

#[derive(Clone)]
pub struct Program {
    pub eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    /// Tail-call chain compilation plan, populated by the optimizer when the
    /// estimated CLIF instruction count exceeds Cranelift's limit.
    pub eval_comb_plan: Option<EvalCombPlan>,
    pub instance_ids: HashMap<InstancePath, InstanceId>,
    pub instance_module: HashMap<InstanceId, ModuleId>,
    pub module_variables: HashMap<ModuleId, HashMap<VarId, VariableInfo>>,
    /// Reverse index: VarPath → VarId for path-based lookups.
    /// `None` marks ambiguous paths (multiple VarIds share the same VarPath).
    pub module_var_path_index: HashMap<ModuleId, HashMap<VarPath, Option<VarId>>>,
    pub module_names: HashMap<ModuleId, StrId>,
    pub clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr>,
    pub topological_clocks: Vec<AbsoluteAddr>,
    pub cascaded_clocks: BTreeSet<AbsoluteAddr>,
    pub arena: SLTNodeArena<AbsoluteAddr>,
    pub num_events: usize,
    /// Maps reset AbsoluteAddr → clock AbsoluteAddr (from FfDeclaration).
    pub reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// Memory layout aliases: non-canonical → canonical address.
    /// Variables with identity Store→Load roundtrips share physical memory.
    pub address_aliases: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// Pre-computed memory layout. Built after optimization, before backend codegen.
    pub layout: Option<crate::backend::MemoryLayout>,
    /// Initial block statements from the top-level module (for native testbenches).
    pub initial_statements: Option<Vec<veryl_analyzer::ir::Statement>>,
    /// Functions defined in the top-level module (for testbench function calls).
    pub tb_functions: fxhash::FxHashMap<veryl_analyzer::ir::VarId, veryl_analyzer::ir::Function>,
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
            .field("num_events", &self.num_events)
            .finish_non_exhaustive()
    }
}

impl Program {
    /// Build and store the memory layout. Also removes identity Stores for
    /// validated aliases and runs DCE to clean up dead instruction chains.
    pub fn build_layout(&mut self, four_state: bool) {
        let layout = crate::backend::MemoryLayout::build(self, four_state);

        // Remove identity Stores for aliases validated by the layout
        if !self.address_aliases.is_empty() {
            let aliased: crate::HashSet<AbsoluteAddr> = self
                .address_aliases
                .iter()
                .filter(|(alias_addr, canonical_addr)| {
                    layout
                        .offsets
                        .get(alias_addr)
                        .zip(layout.offsets.get(canonical_addr))
                        .is_some_and(|(a, c)| a == c)
                })
                .map(|(addr, _)| *addr)
                .collect();
            if !aliased.is_empty() {
                for eu in &mut self.eval_comb {
                    for block in eu.blocks.values_mut() {
                        block.instructions.retain(|inst| {
                            !matches!(
                                inst,
                                SIRInstruction::Store(addr, _, _, _, triggers)
                                    if triggers.is_empty() && aliased.contains(&addr.absolute_addr())
                            )
                        });
                    }
                }
            }
        }

        self.layout = Some(layout);
    }

    pub fn get_addr(
        &self,
        instance_path: &[(&str, usize)],
        var_path: &[&str],
    ) -> Result<AbsoluteAddr, AddrLookupError> {
        let mut instance_path_str_id = Vec::new();
        for path in instance_path {
            let id = veryl_parser::resource_table::insert_str(path.0);
            instance_path_str_id.push((id, path.1));
        }
        let instance_id = *self
            .instance_ids
            .get(&InstancePath(instance_path_str_id))
            .ok_or_else(|| AddrLookupError::InstanceNotFound {
                path: instance_path
                    .iter()
                    .map(|(s, i)| format!("{}[{}]", s, i))
                    .collect::<Vec<_>>()
                    .join("."),
            })?;
        let module_id = self.instance_module[&instance_id];
        let mut var_path_str_id = Vec::new();
        for path in var_path {
            let id = veryl_parser::resource_table::insert_str(path);
            var_path_str_id.push(id);
        }

        let target_path = VarPath(var_path_str_id);
        let path_str = var_path.join(".");
        let entry = self.module_var_path_index[&module_id]
            .get(&target_path)
            .ok_or_else(|| AddrLookupError::VariableNotFound {
                path: path_str.clone(),
            })?;
        let var_id = entry.ok_or_else(|| AddrLookupError::AmbiguousPath { path: path_str })?;
        Ok(AbsoluteAddr {
            instance_id,
            var_id,
        })
    }

    pub fn get_path(&self, addr: &AbsoluteAddr) -> String {
        let instance_id = addr.instance_id;
        let var_id = addr.var_id;

        let instance_path = self
            .instance_ids
            .iter()
            .find(|(_, id)| **id == instance_id)
            .map(|(path, _)| path);
        let module_id = self.instance_module.get(&instance_id).unwrap();
        let module_vars = self.module_variables.get(module_id).unwrap();
        let var_path = module_vars
            .values()
            .find(|info| info.id == var_id)
            .map(|info| &info.path);

        let mut res = Vec::new();
        if let Some(ip) = instance_path {
            for part in &ip.0 {
                res.push(format!(
                    "{}[{}]",
                    veryl_parser::resource_table::get_str_value(part.0).unwrap(),
                    part.1
                ));
            }
        }
        if let Some(vp) = var_path {
            for part in &vp.0 {
                res.push(
                    veryl_parser::resource_table::get_str_value(*part)
                        .unwrap()
                        .to_string(),
                );
            }
        }
        res.join(".")
    }

    pub fn get_variable_info(&self, addr: &AbsoluteAddr) -> Option<&VariableInfo> {
        let module_id = self.instance_module.get(&addr.instance_id)?;
        let module_vars = self.module_variables.get(module_id)?;
        module_vars.get(&addr.var_id)
    }

    pub fn num_events(&self) -> usize {
        self.num_events
    }

    /// Collect the set of `AbsoluteAddr` values that are accessed in the working
    /// region (region != STABLE). These are the only variables that need working
    /// region space.
    pub fn collect_working_region_addrs(&self) -> std::collections::HashSet<AbsoluteAddr> {
        let mut addrs = std::collections::HashSet::new();

        let scan_units =
            |units: &HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
             addrs: &mut std::collections::HashSet<AbsoluteAddr>| {
                for eu_list in units.values() {
                    for eu in eu_list {
                        for block in eu.blocks.values() {
                            for inst in &block.instructions {
                                match inst {
                                    SIRInstruction::Store(addr, _, _, _, _)
                                        if addr.region == WORKING_REGION =>
                                    {
                                        addrs.insert(addr.absolute_addr());
                                    }
                                    SIRInstruction::Commit(src, dst, _, _, _) => {
                                        if src.region == WORKING_REGION {
                                            addrs.insert(src.absolute_addr());
                                        }
                                        if dst.region == WORKING_REGION {
                                            addrs.insert(dst.absolute_addr());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            };

        scan_units(&self.eval_apply_ffs, &mut addrs);
        scan_units(&self.eval_only_ffs, &mut addrs);
        scan_units(&self.apply_ffs, &mut addrs);

        addrs
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct BitAccess {
    pub lsb: usize,
    pub msb: usize,
}
impl BitAccess {
    pub fn new(lsb: usize, msb: usize) -> Self {
        debug_assert!(lsb <= msb, "lsb must be less than or equal to msb");
        Self { lsb, msb }
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        !(self.msb < other.lsb || other.msb < self.lsb)
    }

    /// Calculates the atomic bit ranges for a given access range and a set of boundaries.
    pub fn calculate_atoms(&self, bounds: &BTreeSet<usize>) -> Vec<crate::ir::BitAccess> {
        use std::ops::Bound::*;
        let mut atoms = Vec::new();
        let mut current_lsb = self.lsb;

        // Iterate through the boundaries that are within the access range
        // Excluded(lsb) to Included(msb) handles lsb == msb case naturally (returns empty iterator)
        for &bound in bounds.range((Excluded(self.lsb), Included(self.msb))) {
            atoms.push(crate::ir::BitAccess::new(current_lsb, bound - 1));
            current_lsb = bound;
        }

        // Add the last atom
        if current_lsb <= self.msb {
            atoms.push(crate::ir::BitAccess::new(current_lsb, self.msb));
        }

        atoms
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct VarAtomBase<A> {
    pub id: A,
    pub access: BitAccess,
}
impl<A> VarAtomBase<A> {
    pub fn new(id: A, lsb: usize, msb: usize) -> Self {
        Self {
            id,
            access: BitAccess { lsb, msb },
        }
    }
}
impl fmt::Display for BitAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.lsb == self.msb {
            write!(f, "[{}]", self.lsb)
        } else {
            write!(f, "[{}:{}]", self.msb, self.lsb)
        }
    }
}

impl<A> fmt::Display for VarAtomBase<A>
where
    A: fmt::Display + std::hash::Hash + Eq,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.id, self.access)
    }
}
pub type VarAtom = VarAtomBase<VarId>;
mod builder;
pub(crate) use builder::SIRBuilder;
use veryl_parser::resource_table::StrId;
/// Block identifier
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub usize);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "V: Serialize",
    deserialize = "V: Deserialize<'de> + std::hash::Hash + Eq + Clone"
))]
pub struct GlueBlockBase<V: std::hash::Hash + Eq + Clone> {
    pub module_id: ModuleId,
    pub input_ports: Vec<(Vec<V>, LogicPath<GlueAddrBase<V>>)>,
    pub output_ports: Vec<(Vec<V>, LogicPath<GlueAddrBase<V>>)>,
    pub arena: SLTNodeArena<GlueAddrBase<V>>,
}

/// Concrete glue block using the Veryl analyzer's `VarId`.
pub type GlueBlock = GlueBlockBase<VarId>;
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModuleId(pub usize);

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mod{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceId(pub usize);

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inst{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AbsoluteAddrBase<V> {
    pub instance_id: InstanceId,
    pub var_id: V,
}

/// Concrete address type using the Veryl analyzer's `VarId`.
pub type AbsoluteAddr = AbsoluteAddrBase<VarId>;

impl<V: fmt::Display> fmt::Display for AbsoluteAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AbsoluteAddr({}, {})", self.instance_id, self.var_id)
    }
}

/// A high-performance handle to a signal's physical memory.
///
/// Unlike [`AbsoluteAddr`], which requires a [`HashMap`] lookup for every access,
/// a [`SignalRef`] stores the pre-resolved memory offset and metadata, allowing
/// for essentially zero-cost reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalRef {
    pub offset: usize,
    pub width: usize,
    pub is_4state: bool,
}
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct InstancePath(pub Vec<(StrId, usize)>);

pub const STABLE_REGION: u32 = 0;
pub const WORKING_REGION: u32 = 1;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegionedVarAddrBase<V> {
    pub region: u32,
    pub var_id: V,
}

/// Concrete regioned variable address using the Veryl analyzer's `VarId`.
pub type RegionedVarAddr = RegionedVarAddrBase<VarId>;

impl<V: fmt::Display> fmt::Display for RegionedVarAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedVarAddr(region={}, {})",
            self.region, self.var_id
        )
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegionedAbsoluteAddrBase<V> {
    pub region: u32,
    pub instance_id: InstanceId,
    pub var_id: V,
}

/// Concrete regioned address type using the Veryl analyzer's `VarId`.
pub type RegionedAbsoluteAddr = RegionedAbsoluteAddrBase<VarId>;

impl<V: Copy> RegionedAbsoluteAddrBase<V> {
    pub fn from_absolute_addr(region: u32, addr: AbsoluteAddrBase<V>) -> Self {
        Self {
            region,
            instance_id: addr.instance_id,
            var_id: addr.var_id,
        }
    }

    pub fn absolute_addr(&self) -> AbsoluteAddrBase<V> {
        AbsoluteAddrBase {
            instance_id: self.instance_id,
            var_id: self.var_id,
        }
    }
}

impl<V: fmt::Display> fmt::Display for RegionedAbsoluteAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedAbsoluteAddr(region={}, {}, {})",
            self.region, self.instance_id, self.var_id
        )
    }
}
#[derive(Clone)]
pub struct RelocationModule {
    #[cfg(test)]
    pub variables: HashMap<VarId, Variable>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub comb_blocks: Vec<LogicPath<AbsoluteAddr>>,
}

impl fmt::Debug for RelocationModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ds = f.debug_struct("RelocationModule");
        #[cfg(test)]
        ds.field("variables", &"<omitted>");
        ds.field("eval_apply_ff_blocks", &self.eval_apply_ff_blocks)
            .field("eval_only_ff_blocks", &self.eval_only_ff_blocks)
            .field("apply_ff_blocks", &self.apply_ff_blocks)
            .field("comb_blocks", &self.comb_blocks)
            .finish()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "A: Serialize", deserialize = "A: Deserialize<'de>"))]
pub struct ExecutionUnit<A> {
    pub entry_block_id: BlockId,
    pub blocks: HashMap<BlockId, BasicBlock<A>>,
    pub register_map: HashMap<RegisterId, RegisterType>,
}

impl<A: Display> Display for ExecutionUnit<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ExecutionUnit {{")?;
        writeln!(f, "  entry: b{}", self.entry_block_id.0)?;
        writeln!(f, "  registers: {{")?;
        let mut reg_ids: Vec<_> = self.register_map.keys().collect();
        reg_ids.sort();
        for id in reg_ids {
            let ty = &self.register_map[id];
            match ty {
                RegisterType::Logic { width } => {
                    writeln!(f, "    r{}: logic<{}>", id.0, width)?;
                }
                RegisterType::Bit { width, signed } => {
                    let s = if *signed { "signed " } else { "" };
                    writeln!(f, "    r{}: {}bit<{}>", id.0, s, width)?;
                }
            }
        }
        writeln!(f, "  }}")?;
        let mut block_ids: Vec<_> = self.blocks.keys().collect();
        block_ids.sort();
        for id in block_ids {
            let block = &self.blocks[id];
            writeln!(f, "{}", block)?;
        }
        writeln!(f, "}}")
    }
}
#[derive(Clone)]
pub struct SimModule {
    pub name: StrId,
    pub variables: HashMap<VarId, Variable>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    pub comb_blocks: Vec<LogicPath<VarId>>,
    pub comb_boundaries: HashMap<VarId, std::collections::BTreeSet<usize>>,
    pub arena: SLTNodeArena<VarId>,
    pub store: SymbolicStore<VarId>,
    /// Maps reset VarId → clock VarId, derived from FfDeclarations.
    pub reset_clock_map: HashMap<VarId, VarId>,
}

impl fmt::Debug for SimModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SimModule")
            .field("name", &self.name)
            .field("variables", &"<omitted>")
            .field("eval_only_ff_blocks", &self.eval_only_ff_blocks)
            .field("apply_ff_blocks", &self.apply_ff_blocks)
            .field("eval_apply_ff_blocks", &self.eval_apply_ff_blocks)
            .field("glue_blocks", &self.glue_blocks)
            .field("comb_blocks", &self.comb_blocks)
            .field("comb_boundaries", &self.comb_boundaries)
            .field("arena", &self.arena)
            .field("store", &self.store)
            .field("reset_clock_map", &self.reset_clock_map)
            .finish()
    }
}

impl SimModule {
    pub fn find_var_id(&self, path: &VarPath) -> VarId {
        self.variables
            .iter()
            .find(|(_, var)| &var.path == path)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("Variable '{}' not found in module", path))
    }
}

/// Merge multiple SIR ExecutionUnits into a single EU.
/// Each EU's Return becomes a Jump to the next EU's entry block.
/// RegisterIds and BlockIds are renumbered to avoid conflicts.
/// Returns (merged_eu, eu_entry_block_ids) where eu_entry_block_ids[i] is the
/// BlockId of the i-th EU's entry block in the merged EU (for i > 0).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn merge_sir_eus<A: Clone>(units: &[ExecutionUnit<A>]) -> (ExecutionUnit<A>, Vec<BlockId>) {
    if units.len() == 1 {
        return (units[0].clone(), vec![]);
    }

    let mut merged_blocks = crate::HashMap::default();
    let mut merged_regs = crate::HashMap::default();

    // Compute offsets for renumbering
    let mut reg_offset = 0usize;
    let mut block_offset = 0usize;
    let mut reg_offsets = Vec::new();
    let mut block_offsets = Vec::new();

    for eu in units {
        reg_offsets.push(reg_offset);
        block_offsets.push(block_offset);
        let max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        reg_offset += max_reg + 1;
        let max_block = eu.blocks.keys().map(|b| b.0).max().unwrap_or(0);
        block_offset += max_block + 1;
    }

    // Entry block IDs after renumbering
    let entry_blocks: Vec<BlockId> = units
        .iter()
        .enumerate()
        .map(|(i, eu)| BlockId(eu.entry_block_id.0 + block_offsets[i]))
        .collect();

    for (eu_idx, eu) in units.iter().enumerate() {
        let ro = reg_offsets[eu_idx];
        let bo = block_offsets[eu_idx];
        let is_last = eu_idx == units.len() - 1;
        let next_entry = if !is_last {
            Some(entry_blocks[eu_idx + 1])
        } else {
            None
        };

        // Copy registers with offset
        for (&reg_id, reg_type) in &eu.register_map {
            merged_regs.insert(RegisterId(reg_id.0 + ro), reg_type.clone());
        }

        // Copy blocks with renumbering
        for (&block_id, block) in &eu.blocks {
            let new_block_id = BlockId(block_id.0 + bo);
            let r = |reg: RegisterId| RegisterId(reg.0 + ro);
            let b = |bid: BlockId| BlockId(bid.0 + bo);

            let new_params: Vec<RegisterId> = block.params.iter().map(|p| r(*p)).collect();
            let new_insts: Vec<SIRInstruction<A>> = block
                .instructions
                .iter()
                .map(|inst| renumber_sir_inst(inst, ro, bo))
                .collect();

            let new_terminator = match &block.terminator {
                SIRTerminator::Return => {
                    if is_last {
                        SIRTerminator::Return
                    } else {
                        SIRTerminator::Jump(next_entry.unwrap(), vec![])
                    }
                }
                SIRTerminator::Error(code) => SIRTerminator::Error(*code),
                SIRTerminator::Jump(target, args) => {
                    let new_args: Vec<RegisterId> = args.iter().map(|a| r(*a)).collect();
                    SIRTerminator::Jump(b(*target), new_args)
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => SIRTerminator::Branch {
                    cond: r(*cond),
                    true_block: (
                        b(true_block.0),
                        true_block.1.iter().map(|a| r(*a)).collect(),
                    ),
                    false_block: (
                        b(false_block.0),
                        false_block.1.iter().map(|a| r(*a)).collect(),
                    ),
                },
            };

            merged_blocks.insert(
                new_block_id,
                BasicBlock {
                    id: new_block_id,
                    params: new_params,
                    instructions: new_insts,
                    terminator: new_terminator,
                },
            );
        }
    }

    let eu_boundary_blocks: Vec<BlockId> = entry_blocks[1..].to_vec();
    (
        ExecutionUnit {
            entry_block_id: entry_blocks[0],
            blocks: merged_blocks,
            register_map: merged_regs,
        },
        eu_boundary_blocks,
    )
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn renumber_sir_inst<A: Clone>(
    inst: &SIRInstruction<A>,
    ro: usize,
    _bo: usize,
) -> SIRInstruction<A> {
    let r = |reg: RegisterId| RegisterId(reg.0 + ro);
    let off = |o: &SIROffset| match o {
        SIROffset::Static(v) => SIROffset::Static(*v),
        SIROffset::Dynamic(reg) => SIROffset::Dynamic(r(*reg)),
    };

    match inst {
        SIRInstruction::Imm(dst, val) => SIRInstruction::Imm(r(*dst), val.clone()),
        SIRInstruction::Load(dst, addr, offset, width) => {
            SIRInstruction::Load(r(*dst), addr.clone(), off(offset), *width)
        }
        SIRInstruction::Store(addr, offset, width, src, triggers) => {
            SIRInstruction::Store(addr.clone(), off(offset), *width, r(*src), triggers.clone())
        }
        SIRInstruction::Commit(src, dst, offset, width, triggers) => SIRInstruction::Commit(
            src.clone(),
            dst.clone(),
            off(offset),
            *width,
            triggers.clone(),
        ),
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            SIRInstruction::Binary(r(*dst), r(*lhs), *op, r(*rhs))
        }
        SIRInstruction::Unary(dst, op, src) => SIRInstruction::Unary(r(*dst), *op, r(*src)),
        SIRInstruction::Concat(dst, args) => {
            SIRInstruction::Concat(r(*dst), args.iter().map(|a| r(*a)).collect())
        }
        SIRInstruction::Slice(dst, src, offset, width) => {
            SIRInstruction::Slice(r(*dst), r(*src), *offset, *width)
        }
    }
}

/// Basic Block: A sequence of linear instructions and a terminator instruction
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "Addr: Serialize", deserialize = "Addr: Deserialize<'de>"))]
pub struct BasicBlock<Addr> {
    pub id: BlockId,
    pub params: Vec<RegisterId>,
    /// List of side-effect-free operations, Loads, and Stores
    pub instructions: Vec<SIRInstruction<Addr>>,
    /// Where to transition at the end of this block (key for short-circuit evaluation)
    pub terminator: SIRTerminator,
}

impl<A: Display> fmt::Display for BasicBlock<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "b{}:", self.id.0)?;
        if !self.params.is_empty() {
            write!(f, "  params: [")?;
            for (i, param) in self.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "r{}", param.0)?;
            }
            writeln!(f, "]")?;
        }
        for inst in &self.instructions {
            writeln!(f, "  {}", inst)?;
        }
        write!(f, "  {}", self.terminator)
    }
}

/// Terminator instruction: Determines control flow
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SIRTerminator {
    /// Unconditional transition to the next block
    Jump(BlockId, Vec<RegisterId>),
    /// Conditional branch (true_block if cond is non-zero, false_block if zero)
    Branch {
        cond: RegisterId,
        true_block: (BlockId, Vec<RegisterId>),
        false_block: (BlockId, Vec<RegisterId>),
    },
    /// End of module execution
    Return,
    Error(i64),
}

impl fmt::Display for SIRTerminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Helper to format block ID and argument list
        let fmt_target =
            |f: &mut fmt::Formatter<'_>, id: BlockId, args: &[RegisterId]| -> fmt::Result {
                write!(f, "b{}", id.0)?;
                if !args.is_empty() {
                    write!(f, " [")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "r{}", arg.0)?;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            };

        match self {
            SIRTerminator::Jump(block_id, args) => {
                write!(f, "Jump(")?;
                fmt_target(f, *block_id, args)?;
                write!(f, ")")
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                write!(f, "Branch(r{} ? ", cond.0)?;
                fmt_target(f, true_block.0, &true_block.1)?;
                write!(f, " : ")?;
                fmt_target(f, false_block.0, &false_block.1)?;
                write!(f, ")")
            }
            SIRTerminator::Return => write!(f, "Return"),
            SIRTerminator::Error(code) => write!(f, "Error({})", code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegisterType {
    Logic { width: usize },
    Bit { width: usize, signed: bool },
}
impl RegisterType {
    pub fn width(&self) -> usize {
        match self {
            RegisterType::Bit { width, signed: _ } => *width,
            RegisterType::Logic { width } => *width,
        }
    }
    pub fn is_signed(&self) -> bool {
        matches!(
            self,
            RegisterType::Bit {
                width: _,
                signed: true
            }
        )
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegisterId(pub usize);

impl fmt::Display for RegisterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    And,
    Or,
    Xor,
    Shl, // Logical Shift Left (<<)
    Shr, // Logical Shift Right (>>)
    Sar, // Arithmetic Shift Right (>>>)
    Eq,
    Ne,
    LtU,
    LtS, // Less Than (Unsigned / Signed)
    LeU,
    LeS, // Less Equal
    GtU,
    GtS, // Greater Than
    GeU,
    GeS, // Greater Equal
    LogicAnd,
    LogicOr,
    EqWildcard,
    NeWildcard,
}

impl BinaryOp {
    /// Whether the operation is commutative (a op b == b op a).
    pub fn is_commutative(&self) -> bool {
        matches!(
            self,
            BinaryOp::Add
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
        )
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self {
            BinaryOp::Add => "Add",
            BinaryOp::Sub => "Sub",
            BinaryOp::Mul => "Mul",
            BinaryOp::Div => "Div",
            BinaryOp::Rem => "Rem",
            BinaryOp::And => "And",
            BinaryOp::Or => "Or",
            BinaryOp::Xor => "Xor",
            BinaryOp::Shl => "Shl",
            BinaryOp::Shr => "Shr",
            BinaryOp::Sar => "Sar",
            BinaryOp::Eq => "Eq",
            BinaryOp::Ne => "Ne",
            BinaryOp::LtU => "LtU",
            BinaryOp::LtS => "LtS",
            BinaryOp::LeU => "LeU",
            BinaryOp::LeS => "LeS",
            BinaryOp::GtU => "GtU",
            BinaryOp::GtS => "GtS",
            BinaryOp::GeU => "GeU",
            BinaryOp::GeS => "GeS",
            BinaryOp::LogicAnd => "LogicAnd",
            BinaryOp::LogicOr => "LogicOr",
            BinaryOp::EqWildcard => "EqWildcard",
            BinaryOp::NeWildcard => "NeWildcard",
        };
        write!(f, "{}", op_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    Ident,
    Minus,
    BitNot,
    LogicNot,
    And,
    Or,
    Xor,
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self {
            UnaryOp::Ident => "Ident",
            UnaryOp::Minus => "Minus",
            UnaryOp::BitNot => "BitNot",
            UnaryOp::LogicNot => "LogicNot",
            UnaryOp::And => "And",
            UnaryOp::Or => "Or",
            UnaryOp::Xor => "Xor",
        };
        write!(f, "{}", op_str)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]

pub struct SIRValue {
    #[serde(with = "crate::serde_helpers::biguint")]
    pub payload: BigUint,
    #[serde(with = "crate::serde_helpers::biguint")]
    pub mask: BigUint,
}
impl SIRValue {
    pub fn new(payload: impl Into<BigUint>) -> Self {
        Self {
            payload: payload.into(),
            mask: BigUint::from(0u32),
        }
    }
    pub fn new_four_state(payload: impl Into<BigUint>, mask: impl Into<BigUint>) -> Self {
        Self {
            payload: payload.into(),
            mask: mask.into(),
        }
    }
}

impl fmt::Display for SIRValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mask == BigUint::from(0u32) {
            write!(f, "SIRValue({:#x})", self.payload)
        } else {
            write!(f, "SIRValue({:#x}, mask={:#x})", self.payload, self.mask)
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SIROffset {
    /// Static bit offset
    Static(usize),
    /// Dynamic bit offset (register value)
    Dynamic(RegisterId),
}

impl fmt::Display for SIROffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SIROffset::Static(val) => write!(f, "{}", val),
            SIROffset::Dynamic(reg) => write!(f, "r{}", reg.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(serialize = "Addr: Serialize", deserialize = "Addr: Deserialize<'de>"))]
pub enum SIRInstruction<Addr> {
    Imm(RegisterId, SIRValue),
    Binary(RegisterId, RegisterId, BinaryOp, RegisterId),
    Unary(RegisterId, UnaryOp, RegisterId),
    Load(RegisterId, Addr, SIROffset, usize),
    Store(Addr, SIROffset, usize, RegisterId, Vec<TriggerIdWithKind>),
    /// Commits a value from `src` region to `dst` region with the same offset/width.
    Commit(Addr, Addr, SIROffset, usize, Vec<TriggerIdWithKind>),
    /// Concatenates multiple registers into a single register.
    /// Order: [MSB, ..., LSB] (First element is most significant)
    Concat(RegisterId, Vec<RegisterId>),
    /// Extracts a bit range from a register: dst = src[offset +: width].
    /// Lowered to O(1) chunk-indexed loads in the CLIF backend.
    Slice(RegisterId, RegisterId, usize, usize), // dst, src, bit_offset, width
}

impl<A: Display> fmt::Display for SIRInstruction<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SIRInstruction::Imm(rd, value) => {
                write!(f, "r{} = {}", rd.0, value)
            }
            SIRInstruction::Binary(rd, rs1, op, rs2) => {
                write!(f, "r{} = r{} {} r{}", rd.0, rs1.0, op, rs2.0)
            }
            SIRInstruction::Unary(rd, op, rs) => {
                write!(f, "r{} = {} r{}", rd.0, op, rs.0)
            }
            SIRInstruction::Load(rd, addr, offset, bits) => {
                write!(
                    f,
                    "r{} = Load(addr={}, offset={}, bits={})",
                    rd.0, addr, offset, bits
                )
            }
            SIRInstruction::Store(addr, offset, op_width, src_reg, triggers) => {
                write!(
                    f,
                    "Store(addr={}, offset={}, src_reg = {}, bits={}, triggers={:?})",
                    addr, offset, src_reg.0, op_width, triggers
                )
            }
            SIRInstruction::Commit(src, dst, offset, bits, triggers) => {
                write!(
                    f,
                    "Commit(src={}, dst={}, offset={}, bits={}, triggers={:?})",
                    src, dst, offset, bits, triggers
                )
            }
            SIRInstruction::Concat(dst, args) => {
                write!(f, "r{} = Concat([", dst.0)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "r{}", arg.0)?;
                }
                write!(f, "])")
            }
            SIRInstruction::Slice(dst, src, offset, width) => {
                write!(
                    f,
                    "r{} = Slice(r{}, offset={}, width={})",
                    dst.0, src.0, offset, width
                )
            }
        }
    }
}
impl<A> SIRInstruction<A> {
    pub fn into_map_addr<B>(self, mut f: impl FnMut(A) -> B) -> SIRInstruction<B> {
        match self {
            SIRInstruction::Imm(register_id, value) => SIRInstruction::Imm(register_id, value),
            SIRInstruction::Binary(rd, rs1, op, rs2) => SIRInstruction::Binary(rd, rs1, op, rs2),
            SIRInstruction::Unary(rd, op, rs) => SIRInstruction::Unary(rd, op, rs),
            SIRInstruction::Load(rd, addr, offset, bits) => {
                SIRInstruction::Load(rd, f(addr), offset, bits)
            }
            SIRInstruction::Store(addr, offset, bits, rs, triggers) => {
                SIRInstruction::Store(f(addr), offset, bits, rs, triggers)
            }
            SIRInstruction::Commit(src, dst, offset, bits, triggers) => {
                SIRInstruction::Commit(f(src), f(dst), offset, bits, triggers)
            }
            SIRInstruction::Concat(dst, args) => SIRInstruction::Concat(dst, args),
            SIRInstruction::Slice(dst, src, offset, width) => {
                SIRInstruction::Slice(dst, src, offset, width)
            }
        }
    }
    pub fn map_addr<B>(&self, mut f: impl FnMut(&A) -> B) -> SIRInstruction<B> {
        match self {
            SIRInstruction::Imm(register_id, value) => {
                SIRInstruction::Imm(*register_id, value.clone())
            }
            SIRInstruction::Binary(rd, rs1, op, rs2) => {
                SIRInstruction::Binary(*rd, *rs1, *op, *rs2)
            }
            SIRInstruction::Unary(rd, op, rs) => SIRInstruction::Unary(*rd, *op, *rs),
            SIRInstruction::Load(rd, addr, offset, bits) => {
                SIRInstruction::Load(*rd, f(addr), offset.clone(), *bits)
            }
            SIRInstruction::Store(addr, offset, bits, rs, triggers) => {
                SIRInstruction::Store(f(addr), offset.clone(), *bits, *rs, triggers.clone())
            }
            SIRInstruction::Commit(src, dst, offset, bits, triggers) => {
                SIRInstruction::Commit(f(src), f(dst), offset.clone(), *bits, triggers.clone())
            }
            SIRInstruction::Concat(dst, args) => SIRInstruction::Concat(*dst, args.clone()),
            SIRInstruction::Slice(dst, src, offset, width) => {
                SIRInstruction::Slice(*dst, *src, *offset, *width)
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GlueAddrBase<V> {
    Parent(V),
    Child(V),
}

/// Concrete glue address type using the Veryl analyzer's `VarId`.
pub type GlueAddr = GlueAddrBase<VarId>;

impl<V: Copy> GlueAddrBase<V> {
    pub fn var_id(&self) -> V {
        match self {
            GlueAddrBase::Parent(v) | GlueAddrBase::Child(v) => *v,
        }
    }
}

impl<V: fmt::Display> fmt::Display for GlueAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlueAddrBase::Parent(v) => write!(f, "GlueAddr::Parent({})", v),
            GlueAddrBase::Child(v) => write!(f, "GlueAddr::Child({})", v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sirvalue_display() {
        let val = SIRValue::new(42u64);
        let display = format!("{}", val);
        assert!(display.contains("SIRValue"));
        assert!(display.contains("0x2a")); // 42 in hex
    }

    #[test]
    fn test_absoluteaddr_display() {
        let addr = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        };
        let display = format!("{}", addr);
        assert!(display.contains("AbsoluteAddr"));
        assert!(display.contains("inst0"));
        assert!(display.contains("var0"));
    }

    #[test]
    fn test_glueaddr_display() {
        let parent_addr = GlueAddr::Parent(VarId::default());
        let parent_display = format!("{}", parent_addr);
        assert!(parent_display.contains("GlueAddr::Parent"));
        assert!(parent_display.contains("var0"));

        let child_addr = GlueAddr::Child(VarId::default());
        let child_display = format!("{}", child_addr);
        assert!(child_display.contains("GlueAddr::Child"));
        assert!(child_display.contains("var0"));
    }

    #[test]
    fn test_instanceid_display() {
        let id = InstanceId(42);
        let display = format!("{}", id);
        assert_eq!(display, "inst42");
    }

    #[test]
    fn test_binaryop_display() {
        assert_eq!(format!("{}", BinaryOp::Add), "Add");
        assert_eq!(format!("{}", BinaryOp::Sub), "Sub");
        assert_eq!(format!("{}", BinaryOp::Mul), "Mul");
        assert_eq!(format!("{}", BinaryOp::Xor), "Xor");
    }

    #[test]
    fn test_unaryop_display() {
        assert_eq!(format!("{}", UnaryOp::Minus), "Minus");
        assert_eq!(format!("{}", UnaryOp::LogicNot), "LogicNot");
        assert_eq!(format!("{}", UnaryOp::BitNot), "BitNot");
    }

    #[test]
    fn test_sirinstruction_display() {
        // Test Imm instruction
        let imm: SIRInstruction<i32> = SIRInstruction::Imm(RegisterId(0), SIRValue::new(42u64));
        let imm_display = format!("{}", imm);
        assert!(imm_display.contains("r0"));
        assert!(imm_display.contains("SIRValue"));

        // Test Binary instruction
        let binary: SIRInstruction<i32> =
            SIRInstruction::Binary(RegisterId(0), RegisterId(1), BinaryOp::Add, RegisterId(2));
        let binary_display = format!("{}", binary);
        assert!(binary_display.contains("r0"));
        assert!(binary_display.contains("r1"));
        assert!(binary_display.contains("r2"));
        assert!(binary_display.contains("Add"));

        // Test Unary instruction
        let unary: SIRInstruction<i32> =
            SIRInstruction::Unary(RegisterId(0), UnaryOp::Minus, RegisterId(1));
        let unary_display = format!("{}", unary);
        assert!(unary_display.contains("r0"));
        assert!(unary_display.contains("r1"));
        assert!(unary_display.contains("Minus"));
    }

    #[test]
    fn test_sirterminator_display() {
        // Test Jump
        let jump = SIRTerminator::Jump(BlockId(1), vec![RegisterId(0), RegisterId(1)]);
        let jump_display = format!("{}", jump);
        assert!(jump_display.contains("Jump"));
        assert!(jump_display.contains("b1"));

        // Test Return
        let ret = SIRTerminator::Return;
        let ret_display = format!("{}", ret);
        assert_eq!(ret_display, "Return");

        // Test Branch
        let branch = SIRTerminator::Branch {
            cond: RegisterId(0),
            true_block: (BlockId(1), vec![]),
            false_block: (BlockId(2), vec![]),
        };
        let branch_display = format!("{}", branch);
        assert!(branch_display.contains("Branch"));
        assert!(branch_display.contains("b1"));
        assert!(branch_display.contains("b2"));
    }

    #[test]
    fn test_basicblock_display() {
        let _block: BasicBlock<i32> = BasicBlock {
            id: BlockId(0),
            params: vec![RegisterId(0), RegisterId(1)],
            instructions: vec![
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(42u64)),
                SIRInstruction::Binary(RegisterId(3), RegisterId(0), BinaryOp::Add, RegisterId(2)),
            ],
            terminator: SIRTerminator::Return,
        };

        let block_display = format!("{}", _block);
        assert!(block_display.contains("b0:"));
        assert!(block_display.contains("params:"));
        assert!(block_display.contains("r0"));
        assert!(block_display.contains("r1"));
        assert!(block_display.contains("Add"));
        assert!(block_display.contains("Return"));
    }
}
