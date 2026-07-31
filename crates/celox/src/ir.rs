use crate::{
    HashMap, HashSet,
    logic_tree::{LogicPath, SLTNodeArena, SymbolicStore},
};
pub use celox_design::PortTypeKind;
pub(crate) use celox_design::{
    AbsoluteAddrBase, BinaryOp, BitAccess, DomainKind, InitialStateData, InitialStateValue,
    InitialStateWriteRun, InstanceId, ModuleId, RegionedAbsoluteAddrBase, RegionedVarAddrBase,
    RuntimeEventKind, RuntimeEventSite, RuntimeSchema, SPARSE_WORKING_REGION, STABLE_REGION,
    TriggerIdWithKind, TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION,
};
pub use celox_frontend_veryl::{InstancePath, VariableInfo, VerylFrontendLookup};
pub(crate) use celox_sir::{
    BasicBlock, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRBuilder, SIRInstruction,
    SIROffset, SIRSwitchCase, SIRTerminator, SIRValue, collect_exact_zero_registers, merge_sir_eus,
};
#[cfg(any(target_arch = "x86_64", test))]
pub(crate) use celox_sir::{
    SirMergeProvenance, inline_single_predecessor_jumps, merge_sir_eu_refs_with_provenance,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use veryl_analyzer::ir::{VarId, VarPath, Variable};

/// Concrete address type using the Veryl analyzer's `VarId` during frontend migration.
pub type AbsoluteAddr = AbsoluteAddrBase<VarId>;
/// Concrete regioned variable address using the Veryl analyzer's `VarId`.
pub type RegionedVarAddr = RegionedVarAddrBase<VarId>;
/// Concrete regioned address using the Veryl analyzer's `VarId`.
pub type RegionedAbsoluteAddr = RegionedAbsoluteAddrBase<VarId>;
pub type SirProgram = celox_sir::SirProgram<AbsoluteAddr, RegionedAbsoluteAddr>;

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

pub type InitialMemoryWriteRun = InitialStateWriteRun;
pub type InitialMemoryData = InitialStateData;
pub type InitialMemoryValue = InitialStateValue<AbsoluteAddr>;
pub type ModuleInitialMemoryValue = InitialStateValue<VarId>;
pub type RuntimeErrorInfo<Addr = AbsoluteAddr> = celox_design::RuntimeErrorInfo<Addr>;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LogicPathId(pub usize);

#[derive(Clone, Debug)]
pub struct CombObserver<A = AbsoluteAddr> {
    pub site_id: u32,
    pub activation_group: u32,
    pub guard: Option<crate::logic_tree::NodeId>,
    pub args: Vec<crate::logic_tree::NodeId>,
    pub loop_runner: Option<crate::logic_tree::NodeId>,
    pub sensitivity: Vec<VarAtomBase<A>>,
    pub local_inputs: Vec<(A, crate::logic_tree::NodeId)>,
    pub observed_inputs: Vec<VarAtomBase<A>>,
    pub position_inputs: Vec<VarAtomBase<A>>,
    pub preceding_writes: Vec<VarAtomBase<A>>,
    pub written_before: Vec<VarAtomBase<A>>,
    pub written_input_atoms: Vec<VarAtomBase<A>>,
    pub written_inputs: Vec<A>,
    pub captured_in_loop: bool,
}

#[derive(Clone)]
pub struct Program {
    pub sir: SirProgram,
    pub design: celox_design::ElaboratedDesign<AbsoluteAddr>,
    pub frontend: VerylFrontendLookup,
    pub runtime_schema: RuntimeSchema<AbsoluteAddr>,
    /// Memory layout aliases: non-canonical → canonical address.
    /// Variables with identity Store→Load roundtrips share physical memory.
    pub address_aliases: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// Initial block statements from the top-level module (for native testbenches).
    pub initial_statements: Option<Vec<veryl_analyzer::ir::Statement>>,
    /// Functions defined in the top-level module (for testbench function calls).
    pub tb_functions: fxhash::FxHashMap<veryl_analyzer::ir::VarId, veryl_analyzer::ir::Function>,
}

/// A [`Program`] whose physical state layout has been finalized.
///
/// Backend code generation accepts this artifact instead of a bare `Program`,
/// making it impossible to enter code generation before layout construction.
#[derive(Clone, Debug)]
pub struct LaidOutProgram {
    program: Program,
    layout: crate::backend::MemoryLayout,
}

impl LaidOutProgram {
    pub fn program(&self) -> &Program {
        &self.program
    }

    pub fn layout(&self) -> &crate::backend::MemoryLayout {
        &self.layout
    }

    pub(crate) fn program_mut(&mut self) -> &mut Program {
        &mut self.program
    }

    pub fn into_program(self) -> Program {
        self.program
    }

    pub fn into_parts(self) -> (Program, crate::backend::MemoryLayout) {
        (self.program, self.layout)
    }
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
            .field("num_events", &self.design.events.len())
            .finish_non_exhaustive()
    }
}

impl Program {
    /// Finalize the physical state layout and consume the pre-layout program.
    pub fn into_laid_out(self, four_state: bool) -> LaidOutProgram {
        self.into_laid_out_with_mode(
            four_state,
            crate::backend::memory_layout::MemoryLayoutMode::Packed,
        )
    }

    pub fn into_laid_out_with_mode(
        mut self,
        four_state: bool,
        mode: crate::backend::memory_layout::MemoryLayoutMode,
    ) -> LaidOutProgram {
        if !self.runtime_schema.comb_observers.is_empty() && !self.address_aliases.is_empty() {
            let observed_written: crate::HashSet<AbsoluteAddr> = self
                .runtime_schema
                .comb_observers
                .iter()
                .flat_map(|observer| observer.written_inputs.iter().copied())
                .collect();
            self.address_aliases
                .retain(|alias_addr, _| !observed_written.contains(alias_addr));
            self.address_aliases.retain(|alias_addr, _| {
                !comb_capture_enable_needs_unaliased_old_value(&self.sir.eval_comb, *alias_addr)
            });
        }
        crate::optimizer::coalescing::retain_final_identity_aliases(&mut self, four_state);
        let layout = crate::backend::MemoryLayout::build(&self, four_state, mode);

        // Remove identity Stores for aliases validated by the layout
        if !self.address_aliases.is_empty() {
            let aliased: crate::HashMap<AbsoluteAddr, AbsoluteAddr> = self
                .address_aliases
                .iter()
                .filter(|(alias_addr, canonical_addr)| {
                    layout
                        .offsets
                        .get(alias_addr)
                        .zip(layout.offsets.get(canonical_addr))
                        .is_some_and(|(a, c)| a == c)
                })
                .map(|(&alias, &canonical)| (alias, canonical))
                .collect();
            if !aliased.is_empty() {
                crate::optimizer::coalescing::remove_final_identity_alias_stores(
                    &mut self, &aliased, four_state,
                );
            }
        }

        LaidOutProgram {
            program: self,
            layout,
        }
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
            .frontend
            .instance_ids
            .get(&InstancePath(instance_path_str_id))
            .ok_or_else(|| AddrLookupError::InstanceNotFound {
                path: instance_path
                    .iter()
                    .map(|(s, i)| format!("{}[{}]", s, i))
                    .collect::<Vec<_>>()
                    .join("."),
            })?;
        let module_id = self.frontend.instance_module[&instance_id];
        let mut var_path_str_id = Vec::new();
        for path in var_path {
            let id = veryl_parser::resource_table::insert_str(path);
            var_path_str_id.push(id);
        }

        let target_path = VarPath(var_path_str_id);
        let path_str = var_path.join(".");
        let entry = self.frontend.module_var_path_index[&module_id]
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
            .frontend
            .instance_ids
            .iter()
            .find(|(_, id)| **id == instance_id)
            .map(|(path, _)| path);
        let module_id = self.frontend.instance_module.get(&instance_id).unwrap();
        let module_vars = self.frontend.module_variables.get(module_id).unwrap();
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
        let module_id = self.frontend.instance_module.get(&addr.instance_id)?;
        let module_vars = self.frontend.module_variables.get(module_id)?;
        module_vars.get(&addr.var_id)
    }

    pub fn num_events(&self) -> usize {
        self.design.events.len()
    }

    /// Verify the temporary migration projection from frontend lookup tables
    /// into the source-independent elaborated design. This can be removed once
    /// the frontend tables are consumed rather than retained beside `design`.
    pub(crate) fn verify_design_projection(&self) -> Result<(), String> {
        let expected_count = self
            .frontend
            .instance_module
            .values()
            .map(|module_id| self.frontend.module_variables[module_id].len())
            .sum::<usize>();
        if self.design.state_objects.len() != expected_count {
            return Err(format!(
                "state object count differs: design={} frontend={expected_count}",
                self.design.state_objects.len()
            ));
        }

        for (&instance_id, module_id) in &self.frontend.instance_module {
            for info in self.frontend.module_variables[module_id].values() {
                let address = AbsoluteAddr {
                    instance_id,
                    var_id: info.id,
                };
                let Some(metadata) = self.design.state_objects.get(&address) else {
                    return Err(format!("missing flattened state object {address}"));
                };
                if metadata != &info.metadata {
                    return Err(format!(
                        "metadata differs for flattened state object {address}"
                    ));
                }
            }
        }
        Ok(())
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
                                    SIRInstruction::Store(addr, _, _, _, _, _)
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

        scan_units(&self.sir.eval_apply_ffs, &mut addrs);
        scan_units(&self.sir.eval_comb_apply_ffs, &mut addrs);
        scan_units(&self.sir.eval_only_ffs, &mut addrs);
        scan_units(&self.sir.apply_ffs, &mut addrs);

        addrs
    }

    pub fn collect_sparse_working_region_addrs(&self) -> std::collections::HashSet<AbsoluteAddr> {
        let mut addrs = std::collections::HashSet::new();
        for units in self
            .sir
            .eval_apply_ffs
            .values()
            .chain(self.sir.eval_comb_apply_ffs.values())
            .chain(self.sir.eval_only_ffs.values())
        {
            for eu in units {
                for block in eu.blocks.values() {
                    for inst in &block.instructions {
                        if let SIRInstruction::Store(addr, _, _, _, _, _) = inst
                            && addr.region == SPARSE_WORKING_REGION
                        {
                            addrs.insert(addr.absolute_addr());
                        }
                    }
                }
            }
        }
        addrs
    }
}

fn comb_capture_enable_needs_unaliased_old_value(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    alias_addr: AbsoluteAddr,
) -> bool {
    for eu in units {
        for block in eu.blocks.values() {
            let mut last_store = None;
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Store(addr, _, _, _, _, comb_capture_sites) => {
                        let abs = addr.absolute_addr();
                        if abs == alias_addr && !comb_capture_sites.is_empty() {
                            return true;
                        }
                        last_store = Some(abs);
                    }
                    SIRInstruction::CombCaptureEnableIfChanged { sites, .. } => {
                        if !sites.is_empty() && last_store == Some(alias_addr) {
                            return true;
                        }
                        last_store = None;
                    }
                    _ => {
                        last_store = None;
                    }
                }
            }
        }
    }
    false
}

pub type VarAtom = VarAtomBase<VarId>;
pub(crate) mod cfg {
    pub(crate) use celox_sir::cfg::*;
}
pub(crate) mod verify {
    pub(crate) use celox_sir::verify::*;
}
use veryl_parser::resource_table::StrId;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalArrayLayout {
    pub element_width: usize,
    pub element_count: usize,
    pub element_stride: usize,
    pub plane_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalRef {
    pub offset: usize,
    pub width: usize,
    pub is_4state: bool,
    pub array_layout: Option<SignalArrayLayout>,
}
#[derive(Clone)]
pub struct RelocationModule {
    #[cfg(test)]
    pub variables: HashMap<VarId, Variable>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedAbsoluteAddr>>,
    pub comb_blocks: Vec<LogicPath<AbsoluteAddr>>,
    pub comb_observers: Vec<CombObserver<AbsoluteAddr>>,
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
            .field("comb_observers", &self.comb_observers)
            .finish()
    }
}
#[derive(Clone)]
pub struct SimModule {
    pub name: StrId,
    pub variables: HashMap<VarId, Variable>,
    pub ff_access_summaries: HashMap<TriggerSet<VarId>, FfAccessSummary<RegionedVarAddr>>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>,
    pub glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    pub comb_blocks: Vec<LogicPath<VarId>>,
    pub comb_observers: Vec<CombObserver<VarId>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<VarId>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    pub initial_memory_values: Vec<ModuleInitialMemoryValue>,
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
            .field("ff_access_summaries", &self.ff_access_summaries)
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

/// Sparse scheduler-facing memory effects for one same-trigger FF group.
///
/// These ranges describe the event-entry/current-state values consumed while
/// lowering the group and the next-state ranges it may update. They deliberately
/// retain no lowered SIR so the comb scheduler can reason about FF placement
/// before choosing one shared lowering order.
#[derive(Debug, Clone, Default)]
pub struct FfAccessSummary<A> {
    pub reads: Vec<VarAtomBase<A>>,
    pub writes: Vec<VarAtomBase<A>>,
    pub dynamic_writes: HashSet<A>,
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
    fn exact_zero_analysis_collapses_repeated_concat_dependencies() {
        let zero = RegisterId(0);
        let wide_zero = RegisterId(1);
        let sliced_zero = RegisterId(2);
        let nonzero = RegisterId(3);
        let mixed = RegisterId(4);
        let eu: ExecutionUnit<()> = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                        SIRInstruction::Concat(wide_zero, vec![zero; 4096]),
                        SIRInstruction::Slice(sliced_zero, wide_zero, 0, 64),
                        SIRInstruction::Imm(nonzero, SIRValue::new(1u8)),
                        SIRInstruction::Concat(mixed, vec![zero, nonzero]),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: HashMap::default(),
        };

        let zeros = collect_exact_zero_registers(&eu, [sliced_zero, mixed]);
        assert!(zeros.contains(&zero));
        assert!(zeros.contains(&wide_zero));
        assert!(zeros.contains(&sliced_zero));
        assert!(!zeros.contains(&nonzero));
        assert!(!zeros.contains(&mixed));
    }

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
        assert_eq!(format!("{}", UnaryOp::PopCount), "PopCount");
        assert_eq!(
            format!("{}", UnaryOp::CountLeadingZeros),
            "CountLeadingZeros"
        );
        assert_eq!(
            format!("{}", UnaryOp::CountTrailingZeros),
            "CountTrailingZeros"
        );
    }

    #[test]
    fn bit_count_result_width_represents_operand_width() {
        for (operand_width, expected) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 2),
            (8, 4),
            (usize::MAX, usize::BITS as usize),
        ] {
            for op in [
                UnaryOp::PopCount,
                UnaryOp::CountLeadingZeros,
                UnaryOp::CountTrailingZeros,
            ] {
                assert_eq!(op.result_width(operand_width), expected, "{op}");
            }
        }
    }

    #[test]
    fn bit_count_unary_ops_roundtrip_through_serde() {
        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            let encoded = serde_json::to_string(&op).unwrap();
            let decoded: UnaryOp = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, op);
        }
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

    #[test]
    fn single_predecessor_inlining_rewrites_dominated_parameter_uses() {
        let mut eu: ExecutionUnit<()> = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(0)]),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: vec![RegisterId(1)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ]
            .into_iter()
            .map(|block| (block.id, block))
            .collect(),
            register_map: (0..3)
                .map(|register| {
                    (
                        RegisterId(register),
                        RegisterType::Bit {
                            width: 8,
                            signed: false,
                        },
                    )
                })
                .collect(),
        };
        eu.verify_result().unwrap();

        assert!(inline_single_predecessor_jumps(&mut eu).unwrap());
        eu.verify_result().unwrap();
        assert_eq!(eu.blocks.len(), 1);
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.as_slice(),
            [SIRInstruction::Unary(
                RegisterId(2),
                UnaryOp::Ident,
                RegisterId(0)
            )]
        ));
    }

    #[test]
    fn single_predecessor_inlining_handles_deep_linear_cfg() {
        const BLOCK_COUNT: usize = 20_000;

        let mut eu: ExecutionUnit<()> = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: (0..BLOCK_COUNT)
                .map(|index| {
                    let id = BlockId(index);
                    let terminator = if index + 1 == BLOCK_COUNT {
                        SIRTerminator::Return
                    } else {
                        SIRTerminator::Jump(BlockId(index + 1), Vec::new())
                    };
                    (
                        id,
                        BasicBlock {
                            id,
                            params: Vec::new(),
                            instructions: Vec::new(),
                            terminator,
                        },
                    )
                })
                .collect(),
            register_map: crate::HashMap::default(),
        };
        eu.verify_result().unwrap();

        assert!(inline_single_predecessor_jumps(&mut eu).unwrap());
        assert_eq!(eu.blocks.len(), 1);
        assert_eq!(eu.blocks[&BlockId(0)].terminator, SIRTerminator::Return);
        eu.verify_result().unwrap();
    }
}
