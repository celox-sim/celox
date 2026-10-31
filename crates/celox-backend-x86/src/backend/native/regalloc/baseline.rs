//! Linear register allocation for the baseline native tier.
//!
//! The optimizing allocator plans spills over the whole function and colors
//! SSA live ranges; its analyses grow faster than linearly on the large
//! scheduler and testbench functions. The baseline image only runs until the
//! optimizing image replaces it, so this allocator trades code quality for a
//! few linear walks:
//!
//! - Blocks form extended-basic-block trees: a block with exactly one
//!   predecessor continues its predecessor's tree. Register state flows into
//!   single-predecessor successors and is empty at every tree root.
//! - A value used outside its definition's tree, and every phi, owns a stack
//!   home. Such a value is stored at its definition (a phi by its
//!   predecessors), so every register copy of it is clean. A value whose uses
//!   stay in its tree is stored only when it is evicted while still needed.
//! - Within a block, a backward scan records next uses and a forward scan
//!   assigns registers with farthest-next-use eviction, satisfying fixed uses
//!   and clobbers locally by moves or spills.
//! - Phis become stores to their homes at the end of each predecessor, so the
//!   result has no phis and no edge copies.
//!
//! The result stays strict SSA: a reload or move defines a fresh VReg, and
//! every VReg keeps one register over its live range, which the ordinary
//! assignment verifier checks. [`verify_frame`] separately checks that every
//! frame load reads the value its result stands for.

use crate::HashMap;
use crate::native::features::VariableShiftEncoding;
use crate::native::mir::{
    BaseReg, BlockId, ConstantTableId, MBlock, MFunction, MInst, OpSize, SpillDesc, VReg,
};

use super::RegallocError;
use super::assignment::{
    ALLOCATABLE_REGS, AssignmentMap, PhysReg, RegConstraint, clobbers, is_reg_shift,
    use_constraints,
};
use super::cfg::NormalizedCfg;

const NONE: u32 = u32::MAX;
const NO_REG: u8 = u8::MAX;
const SLOT_BYTES: u32 = 8;
const PHASE: &str = "baseline allocation";

pub(super) struct Allocation {
    pub assignment: AssignmentMap,
    pub spill_frame_size: u32,
    /// Facts for [`verify_frame`], collected only when verification runs.
    pub frame_facts: Option<FrameFacts>,
}

/// The original value each VReg of the result stands for, and the phis the
/// allocation lowered into home stores.
pub(super) struct FrameFacts {
    value_of: Vec<u32>,
    joins: Vec<JoinFacts>,
}

struct JoinFacts {
    block: usize,
    /// `(home offset, phi value, [(predecessor, source value)])`.
    phis: Vec<(i32, u32, Vec<(usize, u32)>)>,
}

fn error(
    rule: &'static str,
    block: Option<BlockId>,
    instruction: Option<usize>,
    message: impl Into<String>,
) -> RegallocError {
    RegallocError::new(PHASE, rule, block, instruction, Vec::new(), message)
}

/// A definition which is cheaper to repeat than to store and reload.
#[derive(Debug, Clone, Copy)]
enum Remat {
    No,
    Imm(u64),
    Table(ConstantTableId),
    Scratch,
}

impl Remat {
    fn of(inst: &MInst) -> Self {
        match *inst {
            MInst::LoadImm { value, .. } => Self::Imm(value),
            MInst::LoadConstantTableAddr { table, .. } => Self::Table(table),
            MInst::Scratch { .. } => Self::Scratch,
            _ => Self::No,
        }
    }

    fn instruction(self, dst: VReg) -> Option<MInst> {
        match self {
            Self::No => None,
            Self::Imm(value) => Some(MInst::LoadImm { dst, value }),
            Self::Table(table) => Some(MInst::LoadConstantTableAddr { dst, table }),
            Self::Scratch => Some(MInst::Scratch { dst }),
        }
    }
}

/// Extended-basic-block trees in allocation order.
struct Trees {
    /// The single predecessor, or `NONE` for a tree root.
    parent: Vec<u32>,
    /// Blocks in depth-first preorder, tree after tree.
    order: Vec<u32>,
    /// Position of each block in `order`.
    pre: Vec<u32>,
    root: Vec<u32>,
    children: Vec<u32>,
}

impl Trees {
    fn build(cfg: &NormalizedCfg) -> Self {
        let blocks = cfg.predecessors.len();
        let parent = cfg
            .predecessors
            .iter()
            .map(|predecessors| match predecessors.as_slice() {
                [only] => *only as u32,
                _ => NONE,
            })
            .collect::<Vec<_>>();
        let mut children = vec![0u32; blocks];
        let mut child_lists = vec![Vec::new(); blocks];
        for block in 0..blocks {
            if parent[block] != NONE {
                children[parent[block] as usize] += 1;
                child_lists[parent[block] as usize].push(block as u32);
            }
        }
        let mut order = Vec::with_capacity(blocks);
        let mut pre = vec![NONE; blocks];
        let mut root = vec![NONE; blocks];
        let mut stack = Vec::new();
        for (tree, &tree_parent) in parent.iter().enumerate() {
            if tree_parent != NONE {
                continue;
            }
            stack.push(tree as u32);
            while let Some(block) = stack.pop() {
                pre[block as usize] = order.len() as u32;
                root[block as usize] = tree as u32;
                order.push(block);
                stack.extend(child_lists[block as usize].iter().rev());
            }
        }
        Self {
            parent,
            order,
            pre,
            root,
            children,
        }
    }
}

/// Per-value facts from one scan of the function, indexed by original VReg.
struct Values {
    /// Single-predecessor phis are replaced by their source.
    subst: Vec<u32>,
    def_block: Vec<u32>,
    phi: Vec<bool>,
    remat: Vec<Remat>,
    /// Used in a tree other than its definition's.
    global: Vec<bool>,
    /// All uses are in the defining block.
    block_local: Vec<bool>,
    /// Largest preorder position of a block using the value.
    last_use_pre: Vec<u32>,
    /// Byte offset of the stack home, or `NONE`.
    home: Vec<u32>,
}

impl Values {
    fn resolve(&self, value: VReg) -> u32 {
        self.subst[value.0 as usize]
    }

    fn needs_home(&self, value: u32) -> bool {
        let value = value as usize;
        (self.global[value] || self.phi[value]) && matches!(self.remat[value], Remat::No)
    }
}

struct Analysis {
    values: Values,
    /// Phi rows stored at the end of each predecessor: `(phi, source)`.
    phi_rows: Vec<Vec<(u32, u32)>>,
    home_bytes: u32,
}

fn analyze(
    func: &MFunction,
    cfg: &NormalizedCfg,
    trees: &Trees,
) -> Result<Analysis, RegallocError> {
    let value_count = func.vregs.count() as usize;
    let blocks = func.blocks.len();
    let mut subst = (0..value_count as u32).collect::<Vec<_>>();
    let mut def_block = vec![NONE; value_count];
    let mut phi = vec![false; value_count];
    let mut remat = vec![Remat::No; value_count];

    let mut renamed = Vec::new();
    for (index, block) in func.blocks.iter().enumerate() {
        let joins = cfg.predecessors[index].len() >= 2;
        for node in &block.phis {
            if joins {
                phi[node.dst.0 as usize] = true;
                def_block[node.dst.0 as usize] = index as u32;
            } else {
                let [(_, source)] = node.sources.as_slice() else {
                    return Err(error(
                        "BASELINE.PHI_SOURCES",
                        Some(block.id),
                        None,
                        format!(
                            "phi {} of a single-predecessor block has several sources",
                            node.dst
                        ),
                    ));
                };
                subst[node.dst.0 as usize] = source.0;
                renamed.push(node.dst.0);
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                def_block[dst.0 as usize] = index as u32;
                remat[dst.0 as usize] = Remat::of(inst);
            }
        }
        if joins && !block.phis.is_empty() {
            for &predecessor in &cfg.predecessors[index] {
                if cfg.successors[predecessor].len() != 1 {
                    return Err(error(
                        "BASELINE.PHI_EDGE_SPLIT",
                        Some(func.blocks[predecessor].id),
                        None,
                        format!(
                            "predecessor of phi block {} has several successors",
                            block.id
                        ),
                    ));
                }
            }
        }
    }
    // Single-predecessor phi chains are acyclic in reachable SSA.
    for &start in &renamed {
        let mut value = start;
        let mut steps = 0;
        while subst[value as usize] != value {
            value = subst[value as usize];
            steps += 1;
            if steps > renamed.len() {
                return Err(error(
                    "BASELINE.PHI_RENAME_CYCLE",
                    None,
                    None,
                    format!("single-predecessor phi v{start} forms a copy cycle"),
                ));
            }
        }
        subst[start as usize] = value;
    }

    let mut values = Values {
        subst,
        def_block,
        phi,
        remat,
        global: vec![false; value_count],
        block_local: vec![false; value_count],
        last_use_pre: vec![0; value_count],
        home: vec![NONE; value_count],
    };
    for value in 0..value_count {
        if values.def_block[value] != NONE {
            values.last_use_pre[value] = trees.pre[values.def_block[value] as usize];
        }
    }
    // RPO index of the last block using each value.
    let mut last_use_block = values.def_block.clone();
    let mut phi_rows = vec![Vec::new(); blocks];
    let mut record = |values: &mut Values, value: u32, block: usize| -> Result<(), RegallocError> {
        let definition = values.def_block[value as usize];
        if definition == NONE {
            return Err(error(
                "BASELINE.USE_WITHOUT_DEF",
                Some(func.blocks[block].id),
                None,
                format!("v{value} is used but never defined"),
            ));
        }
        if trees.root[definition as usize] != trees.root[block] {
            values.global[value as usize] = true;
        }
        let pre = trees.pre[block];
        let last = &mut values.last_use_pre[value as usize];
        *last = (*last).max(pre);
        let last = &mut last_use_block[value as usize];
        *last = (*last).max(block as u32);
        Ok(())
    };
    for (index, block) in func.blocks.iter().enumerate() {
        for inst in &block.insts {
            for used in inst.uses() {
                let value = values.resolve(used);
                record(&mut values, value, index)?;
            }
        }
        if cfg.predecessors[index].len() < 2 {
            continue;
        }
        for node in &block.phis {
            for &(predecessor, source) in &node.sources {
                let Some(&predecessor) = cfg.block_index.get(&predecessor) else {
                    return Err(error(
                        "BASELINE.PHI_PREDECESSOR",
                        Some(block.id),
                        None,
                        format!("phi {} names unknown predecessor {predecessor}", node.dst),
                    ));
                };
                let source = values.resolve(source);
                record(&mut values, source, predecessor)?;
                phi_rows[predecessor].push((node.dst.0, source));
            }
        }
    }
    for value in 0..value_count {
        let definition = values.def_block[value];
        values.block_local[value] = definition != NONE
            && !values.global[value]
            && !values.phi[value]
            && values.last_use_pre[value] == trees.pre[definition as usize];
    }

    // Home intervals over RPO block indices. A phi's home is written at the
    // end of its predecessors and covers them.
    let mut intervals = Vec::new();
    for value in 0..value_count as u32 {
        if !values.needs_home(value) {
            continue;
        }
        let definition = values.def_block[value as usize] as usize;
        let mut start = definition as u32;
        let mut end = last_use_block[value as usize];
        if values.phi[value as usize] {
            for &predecessor in &cfg.predecessors[definition] {
                start = start.min(predecessor as u32);
                end = end.max(predecessor as u32);
            }
        }
        intervals.push((start, end, value));
    }
    let cycle_extent = CycleExtent::new(cfg, blocks);
    for interval in &mut intervals {
        interval.1 = cycle_extent.extend(interval.0, interval.1);
    }
    let slots = color_intervals(blocks, &intervals);
    let mut home_slots = 0u32;
    for (&(_, _, value), &slot) in intervals.iter().zip(&slots) {
        values.home[value as usize] = slot * SLOT_BYTES;
        home_slots = home_slots.max(slot + 1);
    }
    Ok(Analysis {
        values,
        phi_rows,
        home_bytes: home_slots * SLOT_BYTES,
    })
}

/// Extends a block interval over every cycle it may stay live through.
///
/// A value defined in block `d` and used up to block `e` (RPO indices) is
/// live only in blocks on paths from `d` to its uses that do not pass `d`
/// again. Every block on such a path is dominated by `d`, so it lies after
/// `d` in RPO. A forward stretch of the path only increases the RPO index,
/// so a block `z` beyond `e` is followed by a retreating edge `x -> h` with
/// `z <= x`. Following the path's retreating edges back from the use, each
/// target `h` lies in `(d, end]` once the previous source is covered, so
/// raising `end` to the largest retreating source into any block of
/// `(d, end]` until it stops growing covers the live range. This holds for
/// irreducible graphs too, which the compiled scheduler produces.
struct CycleExtent {
    /// `table[k][i]`: largest retreating-edge source into `i..i + 2^k`.
    table: Vec<Vec<u32>>,
}

impl CycleExtent {
    fn new(cfg: &NormalizedCfg, blocks: usize) -> Self {
        let mut extent = (0..blocks as u32).collect::<Vec<_>>();
        let mut retreating = false;
        for (source, successors) in cfg.successors.iter().enumerate() {
            for &target in successors {
                if target <= source {
                    extent[target] = extent[target].max(source as u32);
                    retreating = true;
                }
            }
        }
        if !retreating {
            return Self { table: Vec::new() };
        }
        let mut table = vec![extent];
        let mut width = 1;
        while width * 2 <= blocks {
            let previous = table.last().expect("table has a first level");
            let next = (0..=blocks - width * 2)
                .map(|index| previous[index].max(previous[index + width]))
                .collect();
            table.push(next);
            width *= 2;
        }
        Self { table }
    }

    fn range_max(&self, first: u32, last: u32) -> u32 {
        let (first, last) = (first as usize, last as usize);
        let level = (usize::BITS - 1 - (last - first + 1).leading_zeros()) as usize;
        let row = &self.table[level];
        row[first].max(row[last + 1 - (1 << level)])
    }

    fn extend(&self, start: u32, mut end: u32) -> u32 {
        if self.table.is_empty() {
            return end;
        }
        while start < end {
            let extended = self.range_max(start + 1, end);
            if extended <= end {
                break;
            }
            end = extended;
        }
        end
    }
}

/// Assign the fewest slots to inclusive block intervals by one linear scan.
fn color_intervals(blocks: usize, intervals: &[(u32, u32, u32)]) -> Vec<u32> {
    let mut starting = vec![Vec::new(); blocks];
    let mut ending = vec![Vec::new(); blocks];
    for (index, &(start, end, _)) in intervals.iter().enumerate() {
        starting[start as usize].push(index);
        ending[end as usize].push(index);
    }
    let mut slots = vec![0; intervals.len()];
    let mut free = Vec::new();
    let mut next = 0;
    for block in 0..blocks {
        for &interval in &starting[block] {
            slots[interval] = free.pop().unwrap_or_else(|| {
                next += 1;
                next - 1
            });
        }
        for &interval in &ending[block] {
            free.push(slots[interval]);
        }
    }
    slots
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    value: u32,
    name: u32,
    /// Index of the next use in the current block, or `NONE`.
    next: u32,
    /// Its only copy is this register.
    dirty: bool,
}

const EMPTY: Entry = Entry {
    value: NONE,
    name: NONE,
    next: NONE,
    dirty: false,
};

#[derive(Clone)]
struct RegisterFile {
    entries: [Entry; 16],
    occupied: u16,
}

struct Allocator<'a> {
    values: &'a Values,
    spill_descs: &'a [SpillDesc],
    registers: &'a [PhysReg],
    allocatable: u16,
    shift_encoding: VariableShiftEncoding,
    file: RegisterFile,
    location: Vec<u8>,
    /// Register of every VReg of the result, original and new.
    name_reg: Vec<u8>,
    /// Original value of each new VReg, starting at `first_new`.
    new_values: Vec<u32>,
    new_descs: Vec<SpillDesc>,
    first_new: u32,
    tree_base: u32,
    tree_slot: Vec<u32>,
    tree_next: u32,
    tree_free: Vec<u32>,
    tree_slots: u32,
    next_pos: Vec<u32>,
    current: usize,
    current_id: BlockId,
    current_pre: u32,
    out: Vec<MInst>,
}

fn bit(register: u8) -> u16 {
    1 << register
}

fn registers_in(mask: u16) -> impl Iterator<Item = u8> {
    (0..16u8).filter(move |&register| mask & bit(register) != 0)
}

impl Allocator<'_> {
    fn internal(&self, rule: &'static str, message: impl Into<String>) -> RegallocError {
        error(rule, Some(self.current_id), None, message)
    }

    fn needed(&self, entry: &Entry) -> bool {
        entry.next != NONE || self.values.last_use_pre[entry.value as usize] > self.current_pre
    }

    fn new_name(&mut self, value: u32, register: u8) -> u32 {
        let name = self.first_new + self.new_values.len() as u32;
        self.new_values.push(value);
        self.new_descs
            .push(self.spill_descs[value as usize].copy_for_snapshot());
        self.name_reg.push(register);
        name
    }

    fn install(&mut self, register: u8, entry: Entry) {
        debug_assert_eq!(self.file.occupied & bit(register), 0);
        self.location[entry.value as usize] = register;
        self.file.entries[register as usize] = entry;
        self.file.occupied |= bit(register);
    }

    fn remove(&mut self, register: u8) -> Entry {
        let entry = self.file.entries[register as usize];
        self.location[entry.value as usize] = NO_REG;
        self.file.entries[register as usize] = EMPTY;
        self.file.occupied &= !bit(register);
        entry
    }

    /// Drop a register whose value is no longer needed on this path.
    fn release(&mut self, register: u8) {
        let entry = self.remove(register);
        let value = entry.value as usize;
        if self.values.block_local[value] && self.tree_slot[value] != NONE {
            self.tree_free.push(self.tree_slot[value]);
            self.tree_slot[value] = NONE;
        }
    }

    fn slot_offset(&mut self, value: u32) -> u32 {
        let home = self.values.home[value as usize];
        if home != NONE {
            return home;
        }
        let slot = &mut self.tree_slot[value as usize];
        if *slot == NONE {
            *slot = if self.values.block_local[value as usize] {
                self.tree_free.pop()
            } else {
                None
            }
            .unwrap_or_else(|| {
                self.tree_next += 1;
                self.tree_slots = self.tree_slots.max(self.tree_next);
                self.tree_next - 1
            });
        }
        self.tree_base + *slot * SLOT_BYTES
    }

    fn scratch_slot(&mut self) -> u32 {
        self.tree_next += 1;
        self.tree_slots = self.tree_slots.max(self.tree_next);
        self.tree_base + (self.tree_next - 1) * SLOT_BYTES
    }

    fn store(&mut self, offset: u32, name: u32) {
        self.out.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: offset as i32,
            src: VReg(name),
            size: OpSize::S64,
        });
    }

    /// Free a register, storing its value first if this is its only copy.
    fn evict(&mut self, register: u8) {
        let entry = self.file.entries[register as usize];
        if !self.needed(&entry) {
            self.release(register);
            return;
        }
        if entry.dirty && matches!(self.values.remat[entry.value as usize], Remat::No) {
            let offset = self.slot_offset(entry.value);
            self.store(offset, entry.name);
        }
        self.remove(register);
    }

    /// The occupied register whose next use is farthest, preferring clean.
    fn victim(&self, forbidden: u16) -> Option<u8> {
        registers_in(self.file.occupied & self.allocatable & !forbidden).max_by_key(|&register| {
            let entry = &self.file.entries[register as usize];
            let distance = if self.needed(entry) { entry.next } else { NONE };
            (distance, !entry.dirty)
        })
    }

    /// A free register outside `forbidden`, preferring one outside `avoid`.
    fn free_register(&self, forbidden: u16, avoid: u16) -> Option<u8> {
        let free = self.allocatable & !self.file.occupied & !forbidden;
        let pick = |mask: u16| {
            self.registers
                .iter()
                .map(|&register| register as u8)
                .find(|&register| mask & bit(register) != 0)
        };
        pick(free & !avoid).or_else(|| pick(free))
    }

    fn take_register(&mut self, forbidden: u16, avoid: u16) -> Result<u8, RegallocError> {
        if let Some(register) = self.free_register(forbidden, avoid) {
            return Ok(register);
        }
        let register = self.victim(forbidden).ok_or_else(|| {
            self.internal(
                "BASELINE.NO_REGISTER",
                "every allocatable register is reserved by one instruction",
            )
        })?;
        self.evict(register);
        Ok(register)
    }

    /// Bring a value into a register, reloading or rematerializing it.
    fn ensure(&mut self, value: u32, forbidden: u16) -> Result<u8, RegallocError> {
        let register = self.location[value as usize];
        if register != NO_REG {
            return Ok(register);
        }
        let register = self.take_register(forbidden, 0)?;
        let name = self.new_name(value, register);
        let remat = self.values.remat[value as usize];
        if let Some(inst) = remat.instruction(VReg(name)) {
            self.out.push(inst);
        } else {
            let home = self.values.home[value as usize];
            let slot = self.tree_slot[value as usize];
            let offset = if home != NONE {
                home
            } else if slot != NONE {
                self.tree_base + slot * SLOT_BYTES
            } else {
                return Err(self.internal(
                    "BASELINE.VALUE_UNAVAILABLE",
                    format!("v{value} is neither in a register nor in a stack slot"),
                ));
            };
            self.out.push(MInst::Load {
                dst: VReg(name),
                base: BaseReg::StackFrame,
                offset: offset as i32,
                size: OpSize::S64,
            });
        }
        self.install(
            register,
            Entry {
                value,
                name,
                next: NONE,
                dirty: false,
            },
        );
        Ok(register)
    }

    /// Copy a register's value into another register under a fresh name.
    fn move_entry(&mut self, from: u8, to: u8) {
        let entry = self.remove(from);
        let name = self.new_name(entry.value, to);
        self.out.push(MInst::Mov {
            dst: VReg(name),
            src: VReg(entry.name),
        });
        self.install(to, Entry { name, ..entry });
    }

    fn reset_file(&mut self, file: RegisterFile) {
        for register in registers_in(self.file.occupied) {
            self.location[self.file.entries[register as usize].value as usize] = NO_REG;
        }
        self.file = file;
        for register in registers_in(self.file.occupied) {
            self.location[self.file.entries[register as usize].value as usize] = register;
        }
    }
}

/// Fixed-register operands. Only register shifts have them, and only for the
/// legacy CL encoding; checking the opcode first avoids building the
/// constraint vector for every instruction.
fn fixed_uses(inst: &MInst, encoding: VariableShiftEncoding) -> Vec<(VReg, PhysReg)> {
    if !is_reg_shift(inst) {
        return Vec::new();
    }
    inst.uses()
        .into_iter()
        .zip(use_constraints(inst, encoding))
        .filter_map(|(value, constraint)| match constraint {
            RegConstraint::Fixed(register) => Some((value, register)),
            RegConstraint::Any => None,
        })
        .collect()
}

struct BlockScan {
    /// Distinct operands of each instruction, flattened.
    operands: Vec<u32>,
    start: Vec<u32>,
    /// Next use after each operand position.
    operand_next: Vec<u32>,
    /// First use after each definition.
    def_next: Vec<u32>,
    /// Next use after each phi row of the successor.
    row_next: Vec<u32>,
}

impl BlockScan {
    fn new() -> Self {
        Self {
            operands: Vec::new(),
            start: Vec::new(),
            operand_next: Vec::new(),
            def_next: Vec::new(),
            row_next: Vec::new(),
        }
    }

    /// Record next uses backwards; `next_pos` is left holding each value's
    /// first use in the block.
    fn scan(&mut self, values: &Values, block: &MBlock, rows: &[(u32, u32)], next_pos: &mut [u32]) {
        self.operands.clear();
        self.start.clear();
        for inst in &block.insts {
            self.start.push(self.operands.len() as u32);
            let first = self.operands.len();
            for used in inst.uses() {
                let value = values.resolve(used);
                if !self.operands[first..].contains(&value) {
                    self.operands.push(value);
                }
            }
        }
        self.start.push(self.operands.len() as u32);
        self.operand_next.clear();
        self.operand_next.resize(self.operands.len(), NONE);
        self.def_next.clear();
        self.def_next.resize(block.insts.len(), NONE);
        self.row_next.clear();
        self.row_next.resize(rows.len(), NONE);
        let terminator = block.insts.len() - 1;
        for (index, inst) in block.insts.iter().enumerate().rev() {
            if let Some(dst) = inst.def() {
                self.def_next[index] = next_pos[dst.0 as usize];
                next_pos[dst.0 as usize] = NONE;
            }
            for position in self.start[index] as usize..self.start[index + 1] as usize {
                let value = self.operands[position] as usize;
                self.operand_next[position] = next_pos[value];
                next_pos[value] = index as u32;
            }
            if index == terminator {
                for (row, &(_, source)) in rows.iter().enumerate().rev() {
                    self.row_next[row] = next_pos[source as usize];
                    next_pos[source as usize] = index as u32;
                }
            }
        }
    }

    fn clear(&self, rows: &[(u32, u32)], next_pos: &mut [u32]) {
        for &value in &self.operands {
            next_pos[value as usize] = NONE;
        }
        for &(_, source) in rows {
            next_pos[source as usize] = NONE;
        }
    }
}

pub(super) fn allocate(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    label: &str,
    timing: bool,
    verify: bool,
    is_cancelled: impl Fn() -> bool,
) -> Result<Allocation, RegallocError> {
    let phase = timing.then(crate::timing::now);
    let trees = Trees::build(cfg);
    let analysis = analyze(func, cfg, &trees)?;
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] label={label} baseline analysis trees={} home_bytes={} elapsed={:?}",
            trees
                .order
                .iter()
                .filter(|&&block| trees.parent[block as usize] == NONE)
                .count(),
            analysis.home_bytes,
            start.elapsed()
        );
    }
    if is_cancelled() {
        return Err(super::cancellation_error());
    }

    let phase = timing.then(crate::timing::now);
    let values = &analysis.values;
    let value_count = func.vregs.count();
    let register_count = func.target_features.allocatable_register_count();
    let registers = &ALLOCATABLE_REGS[..register_count];
    let mut allocator = Allocator {
        values,
        spill_descs: &func.spill_descs,
        registers,
        allocatable: registers
            .iter()
            .fold(0, |mask, &register| mask | bit(register as u8)),
        shift_encoding: func.target_features.variable_shift_encoding(),
        file: RegisterFile {
            entries: [EMPTY; 16],
            occupied: 0,
        },
        location: vec![NO_REG; value_count as usize],
        name_reg: vec![NO_REG; value_count as usize],
        new_values: Vec::new(),
        new_descs: Vec::new(),
        first_new: value_count,
        tree_base: analysis.home_bytes,
        tree_slot: vec![NONE; value_count as usize],
        tree_next: 0,
        tree_free: Vec::new(),
        tree_slots: 0,
        next_pos: vec![NONE; value_count as usize],
        current: 0,
        current_id: BlockId(0),
        current_pre: 0,
        out: Vec::new(),
    };
    let mut scan = BlockScan::new();
    let mut saved = vec![None::<(RegisterFile, u32)>; func.blocks.len()];
    let mut rewritten = vec![Vec::new(); func.blocks.len()];
    let mut previous = NONE;
    for (position, &block_index) in trees.order.iter().enumerate() {
        if position % 1024 == 0 && is_cancelled() {
            return Err(super::cancellation_error());
        }
        let block_index = block_index as usize;
        let block = &func.blocks[block_index];
        let successor_rows = match cfg.successors[block_index].as_slice() {
            [successor] if cfg.predecessors[*successor].len() >= 2 => {
                analysis.phi_rows[block_index].as_slice()
            }
            _ => &[],
        };
        allocator.current = block_index;
        allocator.current_id = block.id;
        allocator.current_pre = trees.pre[block_index];
        let parent = trees.parent[block_index];
        if parent == NONE {
            allocator.reset_file(RegisterFile {
                entries: [EMPTY; 16],
                occupied: 0,
            });
            allocator.tree_next = 0;
            allocator.tree_free.clear();
        } else if parent != previous {
            let slot = &mut saved[parent as usize];
            let (file, remaining) = slot.as_mut().ok_or_else(|| {
                error(
                    "BASELINE.TREE_STATE",
                    Some(block.id),
                    None,
                    "a later tree child has no saved parent state",
                )
            })?;
            let file = if *remaining == 1 {
                slot.take().expect("state is present").0
            } else {
                *remaining -= 1;
                file.clone()
            };
            allocator.reset_file(file);
        }
        scan.scan(values, block, successor_rows, &mut allocator.next_pos);
        for register in registers_in(allocator.file.occupied) {
            let value = allocator.file.entries[register as usize].value;
            allocator.file.entries[register as usize].next = allocator.next_pos[value as usize];
            let entry = allocator.file.entries[register as usize];
            if !allocator.needed(&entry) {
                allocator.release(register);
            }
        }
        scan.clear(successor_rows, &mut allocator.next_pos);
        allocator.out = Vec::with_capacity(block.insts.len() + block.insts.len() / 2);
        allocate_block(&mut allocator, block, &scan, successor_rows, cfg)?;
        rewritten[block_index] = std::mem::take(&mut allocator.out);
        if trees.children[block_index] >= 2 {
            saved[block_index] = Some((allocator.file.clone(), trees.children[block_index] - 1));
        }
        previous = block_index as u32;
    }
    let tree_bytes = allocator.tree_slots * SLOT_BYTES;
    let spill_frame_size = analysis
        .home_bytes
        .checked_add(tree_bytes)
        .filter(|&size| size <= i32::MAX as u32)
        .ok_or_else(|| error("BASELINE.FRAME_SIZE", None, None, "spill frame exceeds i32"))?;
    let Allocator {
        name_reg,
        new_values,
        new_descs,
        ..
    } = allocator;
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] label={label} baseline allocate new_vregs={} tree_bytes={tree_bytes} elapsed={:?}",
            new_values.len(),
            start.elapsed()
        );
    }

    let phase = timing.then(crate::timing::now);
    let frame_facts = verify.then(|| {
        let mut value_of = (0..value_count)
            .map(|value| values.resolve(VReg(value)))
            .collect::<Vec<_>>();
        value_of.extend(&new_values);
        let joins = func
            .blocks
            .iter()
            .enumerate()
            .filter(|(index, block)| !block.phis.is_empty() && cfg.predecessors[*index].len() >= 2)
            .map(|(index, block)| JoinFacts {
                block: index,
                phis: block
                    .phis
                    .iter()
                    .map(|node| {
                        (
                            values.home[node.dst.0 as usize] as i32,
                            node.dst.0,
                            node.sources
                                .iter()
                                .map(|&(predecessor, source)| {
                                    (cfg.block_index[&predecessor], values.resolve(source))
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            })
            .collect();
        FrameFacts { value_of, joins }
    });
    for _ in 0..new_values.len() {
        func.vregs.try_alloc().map_err(|exhausted| {
            error("BASELINE.VREG_EXHAUSTED", None, None, exhausted.to_string())
        })?;
    }
    func.spill_descs.extend(new_descs);
    for (block, insts) in func.blocks.iter_mut().zip(rewritten) {
        block.insts = insts;
        block.phis.clear();
    }
    let mut assignment = AssignmentMap::default();
    assignment.map.reserve(name_reg.len());
    for (name, &register) in name_reg.iter().enumerate() {
        if register != NO_REG {
            let register = ALLOCATABLE_REGS
                .iter()
                .copied()
                .find(|&candidate| candidate as u8 == register)
                .expect("allocated register is allocatable");
            assignment.set(VReg(name as u32), register);
        }
    }
    if let Some(start) = phase {
        tracing::debug!(
            "[regalloc-timing] label={label} baseline commit elapsed={:?}",
            start.elapsed()
        );
    }
    Ok(Allocation {
        assignment,
        spill_frame_size,
        frame_facts,
    })
}

fn allocate_block(
    allocator: &mut Allocator<'_>,
    block: &MBlock,
    scan: &BlockScan,
    rows: &[(u32, u32)],
    cfg: &NormalizedCfg,
) -> Result<(), RegallocError> {
    let values = allocator.values;
    let terminator = block.insts.len() - 1;
    let mut operand_registers = Vec::with_capacity(5);
    for (index, inst) in block.insts.iter().enumerate() {
        if index == terminator && !rows.is_empty() {
            store_phi_rows(allocator, scan, rows, cfg)?;
        }
        let operands = &scan.operands[scan.start[index] as usize..scan.start[index + 1] as usize];
        let mut locked = 0u16;
        for &value in operands {
            let register = allocator.ensure(value, locked)?;
            locked |= bit(register);
        }
        let fixed = fixed_uses(inst, allocator.shift_encoding);
        let mut fixed_mask = 0u16;
        for &(operand, required) in &fixed {
            let value = values.resolve(operand);
            let required = required as u8;
            fixed_mask |= bit(required);
            place_fixed(allocator, value, required, operands, &mut locked)?;
        }
        operand_registers.clear();
        for (offset, &value) in operands.iter().enumerate() {
            let register = allocator.location[value as usize];
            operand_registers.push((value, register));
            allocator.file.entries[register as usize].next =
                scan.operand_next[scan.start[index] as usize + offset];
        }
        let mut rewritten = inst.clone();
        let raw = inst.uses();
        for (position, &used) in raw.iter().enumerate() {
            if raw[..position].contains(&used) {
                continue;
            }
            let value = values.resolve(used);
            let name = allocator.file.entries[allocator.location[value as usize] as usize].name;
            if name != used.0 {
                rewritten.rewrite_use(used, VReg(name));
            }
        }

        let clobbered = clobbers(inst)
            .iter()
            .fold(0u16, |mask, &register| mask | bit(register as u8));
        for register in registers_in(clobbered & allocator.file.occupied) {
            let entry = allocator.file.entries[register as usize];
            if !allocator.needed(&entry) {
                allocator.release(register);
            } else if !entry.dirty {
                allocator.remove(register);
            } else if let Some(target) = allocator.free_register(clobbered | locked, 0) {
                allocator.move_entry(register, target);
            } else {
                allocator.evict(register);
            }
        }
        for &(value, _) in &operand_registers {
            let register = allocator.location[value as usize];
            if register != NO_REG {
                let entry = allocator.file.entries[register as usize];
                if !allocator.needed(&entry) {
                    allocator.release(register);
                }
            }
        }

        let Some(dst) = inst.def() else {
            allocator.out.push(rewritten);
            continue;
        };
        let preferred = super::color::definition_preferences(inst)
            .into_iter()
            .flatten()
            .filter_map(|operand| {
                let value = values.resolve(operand);
                operand_registers
                    .iter()
                    .find(|&&(candidate, _)| candidate == value)
                    .map(|&(_, register)| register)
            })
            .find(|&register| {
                allocator.file.occupied & bit(register) == 0 && fixed_mask & bit(register) == 0
            });
        let register = match preferred {
            Some(register) => register,
            None => allocator.take_register(0, fixed_mask)?,
        };
        let value = dst.0;
        let remat = values.remat[value as usize];
        allocator.name_reg[value as usize] = register;
        allocator.out.push(rewritten);
        let home = values.home[value as usize];
        let dirty = home == NONE && matches!(remat, Remat::No);
        if home != NONE {
            allocator.store(home, value);
        }
        let entry = Entry {
            value,
            name: value,
            next: scan.def_next[index],
            dirty,
        };
        allocator.install(register, entry);
        if !allocator.needed(&entry) {
            allocator.release(register);
        }
    }
    Ok(())
}

/// Move `value` into the register a fixed operand requires.
fn place_fixed(
    allocator: &mut Allocator<'_>,
    value: u32,
    required: u8,
    operands: &[u32],
    locked: &mut u16,
) -> Result<(), RegallocError> {
    if allocator.location[value as usize] == required {
        return Ok(());
    }
    if allocator.file.occupied & bit(required) != 0 {
        let occupant = allocator.file.entries[required as usize];
        if operands.contains(&occupant.value) {
            let target = allocator.take_register(*locked | bit(required), 0)?;
            allocator.move_entry(required, target);
            *locked = (*locked & !bit(required)) | bit(target);
        } else if occupant.dirty
            && allocator.needed(&occupant)
            && let Some(target) = allocator.free_register(*locked | bit(required), 0)
        {
            allocator.move_entry(required, target);
        } else {
            allocator.evict(required);
        }
    }
    let from = allocator.location[value as usize];
    allocator.move_entry(from, required);
    *locked = (*locked & !bit(from)) | bit(required);
    Ok(())
}

/// Store a join successor's phi sources into the phis' homes.
///
/// The rows form a parallel copy. A source that is itself a phi of the
/// successor is read before any home is written: it is held in a register,
/// or copied to a scratch slot when registers run out.
fn store_phi_rows(
    allocator: &mut Allocator<'_>,
    scan: &BlockScan,
    rows: &[(u32, u32)],
    cfg: &NormalizedCfg,
) -> Result<(), RegallocError> {
    let values = allocator.values;
    let successor = cfg.successors[allocator.current][0] as u32;
    let mut locked = 0u16;
    let mut scratch = HashMap::<u32, u32>::default();
    let limit = allocator.registers.len().saturating_sub(2) as u32;
    for &(_, source) in rows {
        let aliases = values.phi[source as usize] && values.def_block[source as usize] == successor;
        if !aliases || scratch.contains_key(&source) {
            continue;
        }
        let register = allocator.location[source as usize];
        if register != NO_REG && locked & bit(register) != 0 {
            continue;
        }
        let register = allocator.ensure(source, locked)?;
        if locked.count_ones() < limit {
            locked |= bit(register);
        } else {
            let offset = allocator.scratch_slot();
            let name = allocator.file.entries[register as usize].name;
            allocator.store(offset, name);
            scratch.insert(source, offset);
        }
    }
    for (row, &(phi, source)) in rows.iter().enumerate() {
        let home = values.home[phi as usize];
        if let Some(&offset) = scratch.get(&source) {
            let register = allocator.take_register(locked, 0)?;
            let name = allocator.new_name(source, register);
            allocator.out.push(MInst::Load {
                dst: VReg(name),
                base: BaseReg::StackFrame,
                offset: offset as i32,
                size: OpSize::S64,
            });
            allocator.store(home, name);
            continue;
        }
        let register = allocator.ensure(source, locked)?;
        let name = allocator.file.entries[register as usize].name;
        allocator.store(home, name);
        allocator.file.entries[register as usize].next = scan.row_next[row];
    }
    Ok(())
}

/// Check that every frame load of the result reads the value its VReg
/// stands for on every path, by forward dataflow over frame contents.
pub(super) fn verify_frame(
    func: &MFunction,
    cfg: &NormalizedCfg,
    facts: &FrameFacts,
) -> Result<(), RegallocError> {
    type Frame = HashMap<i32, u32>;
    let blocks = func.blocks.len();
    let mut joins = vec![None; blocks];
    for (index, join) in facts.joins.iter().enumerate() {
        joins[join.block] = Some(index);
    }
    let value_of = |name: VReg| facts.value_of.get(name.0 as usize).copied().unwrap_or(NONE);
    let run = |block: usize, mut frame: Frame, check: bool| -> Result<Frame, RegallocError> {
        for (index, inst) in func.blocks[block].insts.iter().enumerate() {
            match *inst {
                MInst::Store {
                    base: BaseReg::StackFrame,
                    offset,
                    src,
                    size,
                } => {
                    if size != OpSize::S64 || offset % SLOT_BYTES as i32 != 0 {
                        return Err(error(
                            "BASELINE.FRAME_STORE_SHAPE",
                            Some(func.blocks[block].id),
                            Some(index),
                            format!("frame store {inst} is not a full slot"),
                        ));
                    }
                    frame.insert(offset, value_of(src));
                }
                MInst::Load {
                    dst,
                    base: BaseReg::StackFrame,
                    offset,
                    ..
                } if check => {
                    let held = frame.get(&offset).copied();
                    if held != Some(value_of(dst)) {
                        return Err(error(
                            "BASELINE.FRAME_LOAD_VALUE",
                            Some(func.blocks[block].id),
                            Some(index),
                            format!(
                                "{inst} stands for v{} but the slot holds {held:?}",
                                value_of(dst)
                            ),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(frame)
    };
    let entry_frame = |exits: &[Option<Frame>], block: usize| -> Frame {
        let mut merged: Option<Frame> = None;
        for &predecessor in &cfg.predecessors[block] {
            let Some(exit) = &exits[predecessor] else {
                continue;
            };
            let mut incoming = exit.clone();
            if let Some(join) = joins[block] {
                for (home, phi, sources) in &facts.joins[join].phis {
                    let source = sources
                        .iter()
                        .find(|&&(candidate, _)| candidate == predecessor)
                        .map(|&(_, source)| source);
                    if source.is_some() && incoming.get(home).copied() == source {
                        incoming.insert(*home, *phi);
                    } else {
                        incoming.remove(home);
                    }
                }
            }
            merged = Some(match merged {
                None => incoming,
                Some(mut frame) => {
                    frame.retain(|offset, value| incoming.get(offset) == Some(value));
                    frame
                }
            });
        }
        merged.unwrap_or_default()
    };
    let mut exits = vec![None::<Frame>; blocks];
    loop {
        let mut changed = false;
        for block in 0..blocks {
            let exit = run(block, entry_frame(&exits, block), false)?;
            if exits[block].as_ref() != Some(&exit) {
                exits[block] = Some(exit);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for block in 0..blocks {
        run(block, entry_frame(&exits, block), true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
