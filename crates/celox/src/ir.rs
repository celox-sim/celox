use crate::{
    HashMap,
    logic_tree::{LogicPath, SLTNodeArena, SymbolicStore},
};
use malachite_bigint::BigUint;
use std::fmt;
use std::{collections::BTreeSet, fmt::Display};
use veryl_analyzer::ir::{VarId, VarPath, Variable};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TriggerIdWithKind {
    pub kind: DomainKind,
    pub id: usize,
}

#[derive(Clone)]
pub struct VariableInfo {
    pub width: usize,
    pub id: VarId,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TriggerSet<A> {
    pub clock: A,
    pub resets: Vec<A>,
}
#[derive(Debug, Clone)]
pub struct Program {
    pub eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    pub eval_comb: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    pub instance_ids: HashMap<InstancePath, InstanceId>,
    pub instance_module: HashMap<InstanceId, StrId>,
    pub module_variables: HashMap<StrId, HashMap<VarPath, VariableInfo>>,
    pub clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr>,
    pub topological_clocks: Vec<AbsoluteAddr>,
    pub cascaded_clocks: BTreeSet<AbsoluteAddr>,
    pub arena: SLTNodeArena<AbsoluteAddr>,
    pub num_events: usize,
    /// Maps reset AbsoluteAddr → clock AbsoluteAddr (from FfDeclaration).
    pub reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr>,
}
impl Program {
    pub fn get_addr(&self, instance_path: &[(&str, usize)], var_path: &[&str]) -> AbsoluteAddr {
        let mut instance_path_str_id = Vec::new();
        for path in instance_path {
            let id = veryl_parser::resource_table::insert_str(path.0);
            instance_path_str_id.push((id, path.1));
        }
        let instance_id = self.instance_ids[&InstancePath(instance_path_str_id)];
        let module_name = self.instance_module[&instance_id];
        let mut var_path_str_id = Vec::new();
        for path in var_path {
            let id = veryl_parser::resource_table::insert_str(path);
            var_path_str_id.push(id);
        }

        let variable = &self.module_variables[&module_name][&VarPath(var_path_str_id)];
        AbsoluteAddr {
            instance_id,
            var_id: variable.id,
        }
    }

    pub fn get_path(&self, addr: &AbsoluteAddr) -> String {
        let instance_id = addr.instance_id;
        let var_id = addr.var_id;

        let instance_path = self
            .instance_ids
            .iter()
            .find(|(_, id)| **id == instance_id)
            .map(|(path, _)| path);
        let module_name = self.instance_module.get(&instance_id).unwrap();
        let module_vars = self.module_variables.get(module_name).unwrap();
        let var_path = module_vars
            .iter()
            .find(|(_, info)| info.id == var_id)
            .map(|(path, _)| path);

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
        let module_name = self.instance_module.get(&addr.instance_id)?;
        let module_vars = self.module_variables.get(module_name)?;
        module_vars.values().find(|info| info.id == addr.var_id)
    }

    pub fn num_events(&self) -> usize {
        self.num_events
    }

    /// Collect the set of `AbsoluteAddr` values that are accessed in the working
    /// region (region != STABLE). These are the only variables that need working
    /// region space.
    pub fn collect_working_region_addrs(&self) -> std::collections::HashSet<AbsoluteAddr> {
        let mut addrs = std::collections::HashSet::new();

        let scan_units = |units: &HashMap<
            AbsoluteAddr,
            Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
        >,
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
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
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
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
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
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockId(pub usize);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlueBlock {
    pub module_name: StrId,
    pub input_ports: Vec<(Vec<VarId>, LogicPath<GlueAddr>)>,
    pub output_ports: Vec<(Vec<VarId>, LogicPath<GlueAddr>)>,
    pub arena: SLTNodeArena<GlueAddr>,
}
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct InstanceId(pub usize);

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inst{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct AbsoluteAddr {
    pub instance_id: InstanceId,
    pub var_id: VarId,
}

impl fmt::Display for AbsoluteAddr {
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

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegionedVarAddr {
    pub region: u32,
    pub var_id: VarId,
}

impl fmt::Display for RegionedVarAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedVarAddr(region={}, {})",
            self.region, self.var_id
        )
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegionedAbsoluteAddr {
    pub region: u32,
    pub instance_id: InstanceId,
    pub var_id: VarId,
}

impl RegionedAbsoluteAddr {
    pub fn from_absolute_addr(region: u32, addr: AbsoluteAddr) -> Self {
        Self {
            region,
            instance_id: addr.instance_id,
            var_id: addr.var_id,
        }
    }

    pub fn absolute_addr(&self) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: self.instance_id,
            var_id: self.var_id,
        }
    }
}

impl fmt::Display for RegionedAbsoluteAddr {
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
#[derive(Debug, Clone)]
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

/// Basic Block: A sequence of linear instructions and a terminator instruction
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegisterId(pub usize);

impl fmt::Display for RegisterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Eq)]

pub struct SIRValue {
    pub payload: BigUint,
}
impl SIRValue {
    pub fn new(payload: impl Into<BigUint>) -> Self {
        Self {
            payload: payload.into(),
        }
    }
}

impl fmt::Display for SIRValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SIRValue({:#x})", self.payload)
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
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
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlueAddr {
    Parent(VarId),
    Child(VarId),
}
impl GlueAddr {
    pub fn var_id(&self) -> VarId {
        match self {
            GlueAddr::Parent(var_id) => *var_id,
            GlueAddr::Child(var_id) => *var_id,
        }
    }
}

impl fmt::Display for GlueAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlueAddr::Parent(var_id) => write!(f, "GlueAddr::Parent({})", var_id),
            GlueAddr::Child(var_id) => write!(f, "GlueAddr::Child({})", var_id),
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
