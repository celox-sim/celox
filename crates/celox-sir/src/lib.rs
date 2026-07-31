//! Backend-independent Simulator IR for Celox.

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use num_bigint::BigUint;
use num_traits::Zero;
use serde::{Deserialize, Serialize};
use std::{fmt, fmt::Display};

pub use celox_design::{BinaryOp, DomainKind, TriggerIdWithKind, UnaryOp};

pub mod builder;
pub mod cfg;
mod serde_helpers;
pub mod transform;
pub mod verify;

pub use builder::SIRBuilder;
pub use transform::{
    SirMergeProvenance, inline_single_predecessor_jumps, merge_sir_eu_refs,
    merge_sir_eu_refs_with_provenance, merge_sir_eus,
};

/// Block identifier
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        let block_order = celox_analysis::cfg_order::dominance_order(
            self.entry_block_id,
            self.blocks.keys().copied(),
            |id| match &self.blocks[&id].terminator {
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
            },
        );
        for id in block_order {
            let block = &self.blocks[&id];
            writeln!(f, "{}", block)?;
        }
        writeln!(f, "}}")
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SIRTerminator {
    /// Unconditional transition to the next block
    Jump(BlockId, Vec<RegisterId>),
    /// Conditional branch (true_block if cond is non-zero, false_block if zero)
    Branch {
        cond: RegisterId,
        true_block: (BlockId, Vec<RegisterId>),
        false_block: (BlockId, Vec<RegisterId>),
    },
    /// Exact multiway dispatch for a selector of at most eight bits. Cases are
    /// tested against the declared bit width; values not listed take `default`.
    Switch {
        selector: RegisterId,
        cases: Vec<SIRSwitchCase>,
        default: BlockId,
    },
    /// End of module execution
    Return,
    Error(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SIRSwitchCase {
    #[serde(with = "crate::serde_helpers::biguint")]
    pub value: BigUint,
    pub target: BlockId,
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
            SIRTerminator::Switch {
                selector,
                cases,
                default,
            } => {
                write!(f, "Switch(r{}; ", selector.0)?;
                for (index, case) in cases.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{:#x} => ", case.value)?;
                    fmt_target(f, case.target, &[])?;
                }
                write!(f, "; default => ")?;
                fmt_target(f, *default, &[])?;
                write!(f, ")")
            }
            SIRTerminator::Return => write!(f, "Return"),
            SIRTerminator::Error(code) => write!(f, "Error({})", code),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]

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
    /// Access to one element of an unpacked array.
    ///
    /// `index` is the flattened element index, not a bit offset.  The logical
    /// bit offset is `index * element_width + bit_offset`.  Keeping this form
    /// in SIR lets a backend choose an element-strided physical layout without
    /// recovering source type information from arithmetic instructions.
    Element {
        index: RegisterId,
        element_width: usize,
        bit_offset: usize,
        dynamic_bit_offset: Option<RegisterId>,
    },
    /// A vectorized read or write of consecutive unpacked elements through
    /// their packed logical representation.
    ///
    /// Unlike `Static`, this explicitly permits crossing element boundaries.
    /// A backend may require the addressed object to use packed storage.
    PackedElements {
        bit_offset: usize,
        element_width: usize,
    },
}

impl fmt::Display for SIROffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SIROffset::Static(val) => write!(f, "{}", val),
            SIROffset::Dynamic(reg) => write!(f, "r{}", reg.0),
            SIROffset::Element {
                index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            } => {
                write!(
                    f,
                    "element(r{}, width={}, bit={}",
                    index.0, element_width, bit_offset
                )?;
                if let Some(dynamic) = dynamic_bit_offset {
                    write!(f, "+r{}", dynamic.0)?;
                }
                write!(f, ")")
            }
            SIROffset::PackedElements {
                bit_offset,
                element_width,
            } => write!(
                f,
                "packed_elements(bit={}, element_width={})",
                bit_offset, element_width
            ),
        }
    }
}

impl SIROffset {
    /// Returns the constant offset in the object's packed logical bit space.
    ///
    /// `PackedElements` differs from `Static` in the physical layouts a
    /// backend may select, but both name an exact logical range. Analyses
    /// which only reason about aliasing must not treat `PackedElements` as a
    /// dynamic or unknown access.
    pub fn constant_bit_offset(&self) -> Option<usize> {
        match self {
            SIROffset::Static(bit_offset) | SIROffset::PackedElements { bit_offset, .. } => {
                Some(*bit_offset)
            }
            SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
        }
    }

    pub fn dynamic_registers(&self) -> [Option<RegisterId>; 2] {
        match self {
            SIROffset::Static(_) => [None, None],
            SIROffset::Dynamic(register) => [Some(*register), None],
            SIROffset::Element {
                index,
                dynamic_bit_offset,
                ..
            } => [Some(*index), *dynamic_bit_offset],
            SIROffset::PackedElements { .. } => [None, None],
        }
    }

    pub fn is_dynamic(&self) -> bool {
        matches!(self, SIROffset::Dynamic(_) | SIROffset::Element { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(bound(serialize = "Addr: Serialize", deserialize = "Addr: Deserialize<'de>"))]
pub enum SIRInstruction<Addr> {
    Imm(RegisterId, SIRValue),
    Binary(RegisterId, RegisterId, BinaryOp, RegisterId),
    Unary(RegisterId, UnaryOp, RegisterId),
    Load(RegisterId, Addr, SIROffset, usize),
    Store(
        Addr,
        SIROffset,
        usize,
        RegisterId,
        Vec<TriggerIdWithKind>,
        Vec<u32>,
    ),
    /// Commits a value from `src` region to `dst` region with the same offset/width.
    Commit(Addr, Addr, SIROffset, usize, Vec<TriggerIdWithKind>),
    /// Concatenates multiple registers into a single register.
    /// Order: [MSB, ..., LSB] (First element is most significant)
    Concat(RegisterId, Vec<RegisterId>),
    /// Extracts a bit range from a register: dst = src[offset +: width].
    /// Lowered to O(1) chunk-indexed loads in the CLIF backend.
    Slice(RegisterId, RegisterId, usize, usize), // dst, src, bit_offset, width
    /// Mux: dst = if cond { then_val } else { else_val }.
    /// In 4-state mode, preserves exact mask bits (including Z) of the selected branch.
    /// A known one in `cond` selects `then_val`. If `cond` has no known one but
    /// contains X/Z, equal arm bits are preserved and differing bits become X.
    Mux(RegisterId, RegisterId, RegisterId, RegisterId), // dst, cond, then_val, else_val
    RuntimeEvent {
        site_id: u32,
        args: Vec<RegisterId>,
    },
    CombCaptureEvent {
        site_id: u32,
        args: Vec<RegisterId>,
        fatal_error_code: Option<i64>,
        consume_enabled: bool,
    },
    CombCaptureEnableIfChanged {
        old: RegisterId,
        new: RegisterId,
        sites: Vec<u32>,
    },
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
            SIRInstruction::Store(
                addr,
                offset,
                op_width,
                src_reg,
                triggers,
                comb_capture_sites,
            ) => {
                write!(
                    f,
                    "Store(addr={}, offset={}, src_reg = {}, bits={}, triggers={:?}, comb_capture_sites={:?})",
                    addr, offset, src_reg.0, op_width, triggers, comb_capture_sites
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
            SIRInstruction::Mux(dst, cond, then_val, else_val) => {
                write!(
                    f,
                    "r{} = Mux(cond=r{}, then=r{}, else=r{})",
                    dst.0, cond.0, then_val.0, else_val.0
                )
            }
            SIRInstruction::RuntimeEvent { site_id, args } => {
                write!(f, "RuntimeEvent(site={}, args=[", site_id)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "r{}", arg.0)?;
                }
                write!(f, "])")
            }
            SIRInstruction::CombCaptureEvent {
                site_id,
                args,
                fatal_error_code,
                consume_enabled,
            } => {
                write!(f, "CombCaptureEvent(site={}, args=[", site_id)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "r{}", arg.0)?;
                }
                if let Some(code) = fatal_error_code {
                    write!(f, "], fatal_error={code})")
                } else if *consume_enabled {
                    write!(f, "], consume_enabled=true)")
                } else {
                    write!(f, "])")
                }
            }
            SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
                write!(
                    f,
                    "CombCaptureEnableIfChanged(old=r{}, new=r{}, sites={:?})",
                    old.0, new.0, sites
                )
            }
        }
    }
}
impl<A> SIRInstruction<A> {
    pub fn defined_register(&self) -> Option<RegisterId> {
        match self {
            SIRInstruction::Imm(dst, _)
            | SIRInstruction::Binary(dst, _, _, _)
            | SIRInstruction::Unary(dst, _, _)
            | SIRInstruction::Load(dst, _, _, _)
            | SIRInstruction::Concat(dst, _)
            | SIRInstruction::Slice(dst, _, _, _)
            | SIRInstruction::Mux(dst, _, _, _) => Some(*dst),
            SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
        }
    }

    pub fn into_map_addr<B>(self, mut f: impl FnMut(A) -> B) -> SIRInstruction<B> {
        match self {
            SIRInstruction::Imm(register_id, value) => SIRInstruction::Imm(register_id, value),
            SIRInstruction::Binary(rd, rs1, op, rs2) => SIRInstruction::Binary(rd, rs1, op, rs2),
            SIRInstruction::Unary(rd, op, rs) => SIRInstruction::Unary(rd, op, rs),
            SIRInstruction::Load(rd, addr, offset, bits) => {
                SIRInstruction::Load(rd, f(addr), offset, bits)
            }
            SIRInstruction::Store(addr, offset, bits, rs, triggers, comb_capture_sites) => {
                SIRInstruction::Store(f(addr), offset, bits, rs, triggers, comb_capture_sites)
            }
            SIRInstruction::Commit(src, dst, offset, bits, triggers) => {
                SIRInstruction::Commit(f(src), f(dst), offset, bits, triggers)
            }
            SIRInstruction::Concat(dst, args) => SIRInstruction::Concat(dst, args),
            SIRInstruction::Slice(dst, src, offset, width) => {
                SIRInstruction::Slice(dst, src, offset, width)
            }
            SIRInstruction::Mux(dst, cond, then_val, else_val) => {
                SIRInstruction::Mux(dst, cond, then_val, else_val)
            }
            SIRInstruction::RuntimeEvent { site_id, args } => {
                SIRInstruction::RuntimeEvent { site_id, args }
            }
            SIRInstruction::CombCaptureEvent {
                site_id,
                args,
                fatal_error_code,
                consume_enabled,
            } => SIRInstruction::CombCaptureEvent {
                site_id,
                args,
                fatal_error_code,
                consume_enabled,
            },
            SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
                SIRInstruction::CombCaptureEnableIfChanged { old, new, sites }
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
            SIRInstruction::Store(addr, offset, bits, rs, triggers, comb_capture_sites) => {
                SIRInstruction::Store(
                    f(addr),
                    offset.clone(),
                    *bits,
                    *rs,
                    triggers.clone(),
                    comb_capture_sites.clone(),
                )
            }
            SIRInstruction::Commit(src, dst, offset, bits, triggers) => {
                SIRInstruction::Commit(f(src), f(dst), offset.clone(), *bits, triggers.clone())
            }
            SIRInstruction::Concat(dst, args) => SIRInstruction::Concat(*dst, args.clone()),
            SIRInstruction::Slice(dst, src, offset, width) => {
                SIRInstruction::Slice(*dst, *src, *offset, *width)
            }
            SIRInstruction::Mux(dst, cond, then_val, else_val) => {
                SIRInstruction::Mux(*dst, *cond, *then_val, *else_val)
            }
            SIRInstruction::RuntimeEvent { site_id, args } => SIRInstruction::RuntimeEvent {
                site_id: *site_id,
                args: args.clone(),
            },
            SIRInstruction::CombCaptureEvent {
                site_id,
                args,
                fatal_error_code,
                consume_enabled,
            } => SIRInstruction::CombCaptureEvent {
                site_id: *site_id,
                args: args.clone(),
                fatal_error_code: *fatal_error_code,
                consume_enabled: *consume_enabled,
            },
            SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
                SIRInstruction::CombCaptureEnableIfChanged {
                    old: *old,
                    new: *new,
                    sites: sites.clone(),
                }
            }
        }
    }
}

fn visit_exact_zero_dependencies<A>(
    instruction: &SIRInstruction<A>,
    mut visit: impl FnMut(RegisterId),
) -> Option<usize> {
    let mut count = 0usize;
    let mut dependency = |register| {
        count += 1;
        visit(register);
    };
    match instruction {
        SIRInstruction::Binary(
            _,
            lhs,
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Xor
            | BinaryOp::Shl
            | BinaryOp::Shr
            | BinaryOp::Sar,
            rhs,
        ) => {
            dependency(*lhs);
            dependency(*rhs);
        }
        SIRInstruction::Unary(
            _,
            UnaryOp::Ident
            | UnaryOp::ToTwoState
            | UnaryOp::Minus
            | UnaryOp::Or
            | UnaryOp::Xor
            | UnaryOp::PopCount,
            source,
        ) => {
            dependency(*source);
        }
        SIRInstruction::Concat(_, sources) => {
            for &source in sources {
                dependency(source);
            }
        }
        SIRInstruction::Slice(_, source, _, _) => dependency(*source),
        // Equal exact-zero arms make an unknown four-state condition
        // irrelevant too, so the condition is not a dependency.
        SIRInstruction::Mux(_, _, then_value, else_value) => {
            dependency(*then_value);
            dependency(*else_value);
        }
        _ => return None,
    }
    Some(count)
}

/// Prove exact all-zero values reachable from selected roots without
/// materializing their potentially enormous bit representation or building a
/// reverse-use graph for unrelated SIR. The explicit stack also avoids host
/// recursion on deep expression chains.
pub fn collect_exact_zero_registers<A>(
    eu: &ExecutionUnit<A>,
    roots: impl IntoIterator<Item = RegisterId>,
) -> HashSet<RegisterId> {
    let mut definitions = HashMap::<RegisterId, Option<&SIRInstruction<A>>>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            if let Some(dst) = instruction.defined_register() {
                if let Some(definition) = definitions.get_mut(&dst) {
                    *definition = None;
                } else {
                    definitions.insert(dst, Some(instruction));
                }
            }
        }
    }

    let mut result = HashMap::<RegisterId, bool>::default();
    let mut visiting = HashSet::default();
    for root in roots {
        let mut work = vec![(root, false)];
        while let Some((register, expanded)) = work.pop() {
            if result.contains_key(&register) {
                visiting.remove(&register);
                continue;
            }
            let Some(instruction) = definitions.get(&register).copied().flatten() else {
                result.insert(register, false);
                visiting.remove(&register);
                continue;
            };
            if expanded {
                let mut all_zero = true;
                let count = visit_exact_zero_dependencies(instruction, |dependency| {
                    all_zero &= result.get(&dependency) == Some(&true);
                });
                result.insert(register, count.is_some_and(|count| count != 0) && all_zero);
                visiting.remove(&register);
                continue;
            }
            if let SIRInstruction::Imm(_, value) = instruction {
                result.insert(register, value.payload.is_zero() && value.mask.is_zero());
                continue;
            }
            if !visiting.insert(register) {
                result.insert(register, false);
                continue;
            }
            let mut count = 0usize;
            work.push((register, true));
            if visit_exact_zero_dependencies(instruction, |dependency| {
                count += 1;
                if !result.contains_key(&dependency) {
                    work.push((dependency, false));
                }
            })
            .is_none()
                || count == 0
            {
                work.pop();
                result.insert(register, false);
                visiting.remove(&register);
            }
        }
    }
    result
        .into_iter()
        .filter_map(|(register, zero)| zero.then_some(register))
        .collect()
}
