//! Derived clock domains of an event edge as SIR.
//!
//! An edge of an event's signal runs the event's own domain and the domains
//! of the clocks derived from it, with the rules of `celox_runtime`'s
//! `DerivedTickPlan`: clocks computed combinationally from the signal
//! (`clk & en`, `~clk`) are judged from their gate programs at the signal's
//! old and new level and evaluate together with the event's domain; after
//! the commit and a settle, the domains whose clocks moved to their active
//! edge run together, in rounds, until none does, each at most once.
//!
//! [`EdgePlan`] is that plan over the program's units, and
//! [`EdgePlan::emit`] emits one edge as SIR, so that a host can run it
//! without walking the plan in Rust. It covers the plans the Rust walk runs
//! without settling for gates: two-state designs whose gated clocks all have
//! gate programs.

use celox_design::{
    DomainKind, GateOp, SCHEDULER_FLAG_WIDTH, STABLE_REGION, edges::active_level,
    schedule::ScheduleModel,
};
use celox_sir::{BinaryOp, BlockId, SIRBuilder, SIRTerminator, SIRValue, UnaryOp};

use crate::ir::{
    AbsoluteAddr, ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIROffset,
};

type Unit = ExecutionUnit<RegionedAbsoluteAddr>;
type Builder = SIRBuilder<RegionedAbsoluteAddr>;

/// Width of the values a gate program computes with.
const GATE_WIDTH: usize = 64;

/// The deepest stack a gate program may use, as the Rust walk allows.
const GATE_STACK: usize = 16;

/// The most domains one edge can fire: one bit each in a fire mask.
const MAX_ENTRIES: usize = 64;

/// The units of one event domain.
#[derive(Clone, Debug)]
pub(crate) struct DomainUnits {
    eval_apply: Vec<Unit>,
    /// Evaluation without commit; without it, the domain evaluates and
    /// commits at once when it fires with others, as the Rust walk does.
    eval_only: Option<Vec<Unit>>,
    apply: Option<Vec<Unit>>,
}

impl DomainUnits {
    fn of(program: &LaidOutProgram, address: AbsoluteAddr) -> Self {
        let sir = &program.sir;
        Self {
            eval_apply: sir
                .eval_apply_ffs
                .get(&address)
                .cloned()
                .unwrap_or_default(),
            eval_only: sir.eval_only_ffs.get(&address).cloned(),
            apply: sir.apply_ffs.get(&address).cloned(),
        }
    }
}

/// A clock whose edges an edge of the event can cause.
#[derive(Clone, Debug)]
struct Derived {
    signal: AbsoluteAddr,
    width: usize,
    kind: Option<DomainKind>,
    units: DomainUnits,
}

/// A clock computed combinationally from the event's signal.
#[derive(Clone, Debug)]
struct Gated {
    derived: Derived,
    gate: Vec<GateOp<AbsoluteAddr>>,
}

/// The derived domains an edge of one event runs.
#[derive(Clone, Debug)]
pub(crate) struct EdgePlan {
    /// The event's own domain.
    source: DomainUnits,
    /// Domains clocked from state the event's domain writes.
    domains: Vec<Derived>,
    /// Clocks computed combinationally from the event's signal.
    gated: Vec<Gated>,
    /// The gated domains read and write no state the event's domain writes,
    /// nor one another's: at the edge they run first, one at a time.
    independent: bool,
}

impl EdgePlan {
    /// The plan of the canonical event `event`: `Ok(None)` when no clock
    /// derives from it, an error when the plan is one this module does not
    /// emit.
    pub(crate) fn new(
        program: &LaidOutProgram,
        model: &ScheduleModel<AbsoluteAddr>,
        event: AbsoluteAddr,
    ) -> Result<Option<Self>, &'static str> {
        let events = &model.events;
        let domains = events
            .derived_events
            .get(&event)
            .map_or(&[][..], Vec::as_slice);
        let gated = events
            .combinational_derived
            .get(&event)
            .map_or(&[][..], Vec::as_slice);
        if domains.is_empty() && gated.is_empty() {
            return Ok(None);
        }
        let layout = program.layout();
        if layout.four_state {
            return Err("the design is four-state");
        }
        // A host checks these after each unit it runs; a generated edge runs
        // its units without returning in between.
        if !program.runtime.runtime_schema.comb_observers.is_empty() {
            return Err("the design observes combinational processes");
        }
        let comb_fatal = program.sir.eval_comb.iter().any(|unit| {
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .any(|inst| {
                    matches!(
                        inst,
                        SIRInstruction::CombCaptureEvent {
                            fatal_error_code: Some(_),
                            ..
                        }
                    )
                })
        });
        if comb_fatal {
            return Err("combinational logic has fatal assertions");
        }
        if domains.len() + gated.len() + 1 > MAX_ENTRIES {
            return Err("an edge reaches too many derived domains");
        }
        // A derived clock without a domain of its own is left out, as the
        // Rust walk leaves out a clock without an event.
        let derived = |address: AbsoluteAddr| -> Result<Option<Derived>, &'static str> {
            if !program.sir.eval_apply_ffs.contains_key(&address) {
                return Ok(None);
            }
            Ok(Some(Derived {
                signal: address,
                width: *layout
                    .widths
                    .get(&address)
                    .ok_or("a derived clock has no storage")?,
                kind: model.kind(&address),
                units: DomainUnits::of(program, address),
            }))
        };
        let loadable = |address: &AbsoluteAddr, lsb: u32, width: u32| {
            layout
                .widths
                .get(address)
                .is_some_and(|&stored| stored <= GATE_WIDTH && (lsb + width) as usize <= stored)
                && !layout.unpacked_arrays.contains_key(address)
        };
        let mut gated_domains = Vec::with_capacity(gated.len());
        for clock in gated {
            let Some(derived) = derived(clock.clock)? else {
                continue;
            };
            let gate = clock
                .gate
                .clone()
                .ok_or("a gated clock has no gate program")?;
            if !gate_fits(&gate, loadable) {
                return Err("a gated clock has no gate program");
            }
            gated_domains.push(Gated { derived, gate });
        }
        let mut state_domains = Vec::with_capacity(domains.len());
        for &address in domains {
            if let Some(derived) = derived(address)? {
                state_domains.push(derived);
            }
        }
        if state_domains.is_empty() && gated_domains.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            source: DomainUnits::of(program, event),
            domains: state_domains,
            gated: gated_domains,
            independent: events.independent_derived.contains(&event),
        }))
    }

    /// The entries of a fire mask: the event's domain, then the
    /// state-derived domains, then the gated ones.
    fn entries(&self) -> impl Iterator<Item = &DomainUnits> + '_ {
        std::iter::once(&self.source)
            .chain(self.domains.iter().map(|domain| &domain.units))
            .chain(self.gated.iter().map(|gated| &gated.derived.units))
    }

    fn gated_bit(&self, index: usize) -> usize {
        1 + self.domains.len() + index
    }

    /// Whether entry `entry` can fire together with another: the event's own
    /// domain with gated clocks that are not independent, the derived ones
    /// in a round with another derived one or at the edge with the event's
    /// own.
    fn fires_with_others(&self, entry: usize) -> bool {
        let derived = self.domains.len() + self.gated.len();
        let gated_together = !self.gated.is_empty() && !self.independent;
        if entry == 0 {
            gated_together
        } else {
            derived > 1 || (entry > self.domains.len() && gated_together)
        }
    }

    /// Emit one edge of the event's signal, which has already moved from
    /// level `from` to `to` (one-bit registers). `fires` says whether the
    /// event's own domain sees its active edge, `settle` whether
    /// combinational logic must settle before the domains evaluate. The code
    /// continues at `settled` after the domains ran and logic settled, and
    /// at `unsettled` when nothing ran (no gated clock and no own edge).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn emit(
        &self,
        b: &mut Builder,
        comb: &[Unit],
        from: RegisterId,
        to: RegisterId,
        fires: RegisterId,
        settle: RegisterId,
        settled: BlockId,
        unsettled: BlockId,
    ) {
        let mut e = Emitter {
            b,
            after: Vec::new(),
        };
        // The state-derived clocks' levels before the edge, the baseline of
        // their edges for the whole phase.
        let was: Vec<RegisterId> = self
            .domains
            .iter()
            .map(|domain| e.level(domain.signal, domain.width))
            .collect();
        // The fire section's parameters: the domains to fire, those fired so
        // far, whether to settle first, and whether the gated domains run
        // first one at a time (`fire_with_gated`).
        let mask = e.b.alloc_bit(MAX_ENTRIES, false);
        let fired = e.b.alloc_bit(MAX_ENTRIES, false);
        let settle_first = e.b.alloc_bit(1, false);
        let alone = e.b.alloc_bit(1, false);
        let fire = e.b.new_block_with(vec![mask, fired, settle_first, alone]);
        let source = e.bit(fires, 0);
        if self.gated.is_empty() {
            let zero = e.constant(0, MAX_ENTRIES);
            let none = e.binary(1, source, BinaryOp::Eq, zero);
            let no = e.constant(0, 1);
            let start = e.b.new_block();
            e.b.seal_block(SIRTerminator::Branch {
                cond: none,
                true_block: (unsettled, Vec::new()),
                false_block: (start, Vec::new()),
            });
            e.b.switch_to_block(start);
            e.b.seal_block(SIRTerminator::Jump(fire, vec![source, zero, settle, no]));
        } else {
            // The edge: each gated clock judged from its gate program with
            // the signal at its old and new level.
            let mut ready = source;
            let mut gated_fired = e.constant(0, MAX_ENTRIES);
            let mut after = Vec::with_capacity(self.gated.len());
            for (index, gated) in self.gated.iter().enumerate() {
                let before = e.gate(&gated.gate, from);
                let level = e.gate(&gated.gate, to);
                let edge = e.edge(gated.derived.kind, before, level);
                let bit = e.bit(edge, self.gated_bit(index));
                ready = e.binary(MAX_ENTRIES, ready, BinaryOp::Or, bit);
                gated_fired = e.binary(MAX_ENTRIES, gated_fired, BinaryOp::Or, bit);
                after.push(level);
            }
            let alone = e.constant(u64::from(self.independent), 1);
            e.b.seal_block(SIRTerminator::Jump(
                fire,
                vec![ready, gated_fired, settle, alone],
            ));
            e.after = after;
        }

        // The fire section, shared by the edge and the rounds.
        e.b.switch_to_block(fire);
        let fire_end = e.b.new_block();
        e.settle_if(settle_first, comb);
        let zero = e.constant(0, MAX_ENTRIES);
        let gated_mask = self
            .gated
            .iter()
            .enumerate()
            .fold(0u64, |mask, (index, _)| mask | 1 << self.gated_bit(index));
        let gated_bits = e.constant(gated_mask, MAX_ENTRIES);
        let gated_ready = e.binary(MAX_ENTRIES, mask, BinaryOp::And, gated_bits);
        let any_gated = e.binary(1, gated_ready, BinaryOp::Ne, zero);
        let in_turn = e.binary(1, alone, BinaryOp::And, any_gated);
        // Each domain's units appear once in the unit: the one-domain path
        // also serves the independent gated domains one at a time, and only
        // domains that can fire with another get the several-domain path.
        let one_mask = e.b.alloc_bit(MAX_ENTRIES, false);
        let remaining = e.b.alloc_bit(MAX_ENTRIES, false);
        let turn = e.b.alloc_bit(1, false);
        let one_domain = e.b.new_block_with(vec![one_mask, remaining, turn]);
        let rest = e.b.alloc_bit(MAX_ENTRIES, false);
        let next_turn = e.b.new_block_with(vec![rest]);
        let together = e.b.new_block();
        e.b.seal_block(SIRTerminator::Branch {
            cond: in_turn,
            true_block: (next_turn, vec![mask]),
            false_block: (together, Vec::new()),
        });

        // Independent gated domains, then the event's own, each fused, one
        // at a time (`fire_with_gated`).
        e.b.switch_to_block(next_turn);
        let order: Vec<usize> = (0..self.gated.len())
            .map(|index| self.gated_bit(index))
            .chain(std::iter::once(0))
            .collect();
        for entry in order {
            let bit = e.constant(1 << entry, MAX_ENTRIES);
            let selected = e.binary(MAX_ENTRIES, rest, BinaryOp::And, bit);
            let set = e.binary(1, selected, BinaryOp::Ne, zero);
            let take = e.b.new_block();
            let skip = e.b.new_block();
            e.b.seal_block(SIRTerminator::Branch {
                cond: set,
                true_block: (take, Vec::new()),
                false_block: (skip, Vec::new()),
            });
            e.b.switch_to_block(take);
            let all = e.constant(u64::MAX, MAX_ENTRIES);
            let others = e.binary(MAX_ENTRIES, bit, BinaryOp::Xor, all);
            let left = e.binary(MAX_ENTRIES, rest, BinaryOp::And, others);
            let yes = e.constant(1, 1);
            e.b.seal_block(SIRTerminator::Jump(one_domain, vec![bit, left, yes]));
            e.b.switch_to_block(skip);
        }
        e.b.seal_block(SIRTerminator::Jump(fire_end, Vec::new()));

        // The domains together: one evaluates and commits at once, several
        // evaluate, then commit, in entry order.
        e.b.switch_to_block(together);
        let one = e.constant(1, MAX_ENTRIES);
        let lower = e.binary(MAX_ENTRIES, mask, BinaryOp::Sub, one);
        let others = e.binary(MAX_ENTRIES, mask, BinaryOp::And, lower);
        let single = e.binary(1, others, BinaryOp::Eq, zero);
        let several = e.b.new_block();
        let no = e.constant(0, 1);
        let none_left = e.constant(0, MAX_ENTRIES);
        e.b.seal_block(SIRTerminator::Branch {
            cond: single,
            true_block: (one_domain, vec![mask, none_left, no]),
            false_block: (several, Vec::new()),
        });
        e.b.switch_to_block(one_domain);
        for (entry, units) in self.entries().enumerate() {
            e.if_bit(one_mask, entry, |e| e.inline_all(&units.eval_apply));
        }
        e.b.seal_block(SIRTerminator::Branch {
            cond: turn,
            true_block: (next_turn, vec![remaining]),
            false_block: (fire_end, Vec::new()),
        });
        e.b.switch_to_block(several);
        let joint: Vec<bool> = (0..self.domains.len() + self.gated.len() + 1)
            .map(|entry| self.fires_with_others(entry))
            .collect();
        for (entry, units) in self.entries().enumerate() {
            if joint[entry] {
                e.if_bit(mask, entry, |e| match &units.eval_only {
                    Some(eval_only) => e.inline_all(eval_only),
                    None => e.inline_all(&units.eval_apply),
                });
            }
        }
        for (entry, units) in self.entries().enumerate() {
            if let (true, Some(apply)) = (joint[entry], &units.apply) {
                e.if_bit(mask, entry, |e| e.inline_all(apply));
            }
        }
        e.b.seal_block(SIRTerminator::Jump(fire_end, Vec::new()));

        // Combinational logic settles after every fire; then a round: the
        // domains not fired yet whose clocks moved to their active edge.
        e.b.switch_to_block(fire_end);
        e.inline_all(comb);
        let mut ready = e.constant(0, MAX_ENTRIES);
        for (index, domain) in self.domains.iter().enumerate() {
            let is = e.level(domain.signal, domain.width);
            let edge = e.edge(domain.kind, was[index], is);
            let bit = e.bit(edge, 1 + index);
            ready = e.binary(MAX_ENTRIES, ready, BinaryOp::Or, bit);
        }
        let after = std::mem::take(&mut e.after);
        for (index, gated) in self.gated.iter().enumerate() {
            let is = e.gate(&gated.gate, to);
            let edge = e.edge(gated.derived.kind, after[index], is);
            let bit = e.bit(edge, self.gated_bit(index));
            ready = e.binary(MAX_ENTRIES, ready, BinaryOp::Or, bit);
        }
        let all = e.constant(u64::MAX, MAX_ENTRIES);
        let unfired = e.binary(MAX_ENTRIES, fired, BinaryOp::Xor, all);
        let ready = e.binary(MAX_ENTRIES, ready, BinaryOp::And, unfired);
        let quiet = e.binary(1, ready, BinaryOp::Eq, zero);
        let round = e.b.new_block();
        e.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (settled, Vec::new()),
            false_block: (round, Vec::new()),
        });
        e.b.switch_to_block(round);
        let now_fired = e.binary(MAX_ENTRIES, fired, BinaryOp::Or, ready);
        let no = e.constant(0, 1);
        e.b.seal_block(SIRTerminator::Jump(fire, vec![ready, now_fired, no, no]));
    }
}

/// Whether `gate` is a program the emitter runs: its loads read state of at
/// most 64 bits, and its stack stays within the Rust walk's bound and ends
/// with one value.
fn gate_fits(
    gate: &[GateOp<AbsoluteAddr>],
    loadable: impl Fn(&AbsoluteAddr, u32, u32) -> bool,
) -> bool {
    let mut depth = 0usize;
    for op in gate {
        let pops = match op {
            GateOp::Load {
                address,
                lsb,
                width,
            } => {
                if !loadable(address, *lsb, *width) {
                    return false;
                }
                0
            }
            GateOp::Source | GateOp::Const(_) => 0,
            GateOp::Not { .. }
            | GateOp::LogicNot
            | GateOp::ReduceAnd { .. }
            | GateOp::ReduceOr
            | GateOp::ReduceXor
            | GateOp::Slice { .. } => 1,
            GateOp::And
            | GateOp::Or
            | GateOp::Xor
            | GateOp::Eq
            | GateOp::Ne
            | GateOp::LogicAnd
            | GateOp::LogicOr => 2,
            GateOp::Mux => 3,
        };
        let Some(next) = depth.checked_sub(pops) else {
            return false;
        };
        depth = next + 1;
        if depth > GATE_STACK {
            return false;
        }
    }
    depth == 1
}

fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

struct Emitter<'b> {
    b: &'b mut Builder,
    /// The gated clocks' levels just after the edge, the baseline of their
    /// further edges in the rounds.
    after: Vec<RegisterId>,
}

impl Emitter<'_> {
    fn constant(&mut self, value: u64, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    fn binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Binary(register, lhs, op, rhs));
        register
    }

    fn unary(&mut self, width: usize, op: UnaryOp, value: RegisterId) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Unary(register, op, value));
        register
    }

    /// `value` as a gate value, zero-extended.
    fn widen(&mut self, value: RegisterId) -> RegisterId {
        self.unary(GATE_WIDTH, UnaryOp::Ident, value)
    }

    fn load(&mut self, address: AbsoluteAddr, lsb: usize, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Load(
            register,
            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address),
            SIROffset::Static(lsb),
            width,
        ));
        register
    }

    /// The edge level of a signal as the runtime reads it: whether its low
    /// byte, within its width, is not zero.
    fn level(&mut self, signal: AbsoluteAddr, width: usize) -> RegisterId {
        let width = width.min(8);
        let value = self.load(signal, 0, width);
        let zero = self.constant(0, width);
        self.binary(1, value, BinaryOp::Ne, zero)
    }

    /// Whether a change of a domain's clock from `was` to `is` (one-bit
    /// levels) is its active edge, as `settled_edge` judges two-state levels.
    fn edge(&mut self, kind: Option<DomainKind>, was: RegisterId, is: RegisterId) -> RegisterId {
        let changed = self.binary(1, was, BinaryOp::Ne, is);
        match kind {
            Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                self.binary(1, changed, BinaryOp::And, is)
            }
            Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                self.binary(1, changed, BinaryOp::And, was)
            }
            _ => changed,
        }
    }

    /// Bit `index` of a fire mask, set when `condition` holds.
    fn bit(&mut self, condition: RegisterId, index: usize) -> RegisterId {
        let set = self.constant(1 << index, MAX_ENTRIES);
        let unset = self.constant(0, MAX_ENTRIES);
        let bit = self.b.alloc_bit(MAX_ENTRIES, false);
        self.b.emit(SIRInstruction::Mux(bit, condition, set, unset));
        bit
    }

    /// The level of a gated clock from its program, with the event's signal
    /// at `source` (a one-bit register).
    fn gate(&mut self, gate: &[GateOp<AbsoluteAddr>], source: RegisterId) -> RegisterId {
        let mut stack: Vec<RegisterId> = Vec::with_capacity(GATE_STACK);
        for op in gate {
            let value = match *op {
                GateOp::Source => self.widen(source),
                GateOp::Load {
                    address,
                    lsb,
                    width,
                } => {
                    let value = self.load(address, lsb as usize, width as usize);
                    self.widen(value)
                }
                GateOp::Const(value) => self.constant(value, GATE_WIDTH),
                GateOp::Not { width } => {
                    let value = stack.pop().expect("checked by gate_fits");
                    let mask = self.constant(mask(width), GATE_WIDTH);
                    self.binary(GATE_WIDTH, value, BinaryOp::Xor, mask)
                }
                GateOp::LogicNot => {
                    let value = stack.pop().expect("checked by gate_fits");
                    let zero = self.constant(0, GATE_WIDTH);
                    let is = self.binary(1, value, BinaryOp::Eq, zero);
                    self.widen(is)
                }
                GateOp::ReduceAnd { width } => {
                    let value = stack.pop().expect("checked by gate_fits");
                    let mask = self.constant(mask(width), GATE_WIDTH);
                    let bits = self.binary(GATE_WIDTH, value, BinaryOp::And, mask);
                    let is = self.binary(1, bits, BinaryOp::Eq, mask);
                    self.widen(is)
                }
                GateOp::ReduceOr => {
                    let value = stack.pop().expect("checked by gate_fits");
                    let zero = self.constant(0, GATE_WIDTH);
                    let is = self.binary(1, value, BinaryOp::Ne, zero);
                    self.widen(is)
                }
                GateOp::ReduceXor => {
                    let value = stack.pop().expect("checked by gate_fits");
                    let parity = self.unary(1, UnaryOp::Xor, value);
                    self.widen(parity)
                }
                GateOp::Slice { lsb, width } => {
                    let value = stack.pop().expect("checked by gate_fits");
                    let shift = self.constant(u64::from(lsb), GATE_WIDTH);
                    let shifted = self.binary(GATE_WIDTH, value, BinaryOp::Shr, shift);
                    let mask = self.constant(mask(width), GATE_WIDTH);
                    self.binary(GATE_WIDTH, shifted, BinaryOp::And, mask)
                }
                GateOp::Mux => {
                    let otherwise = stack.pop().expect("checked by gate_fits");
                    let then = stack.pop().expect("checked by gate_fits");
                    let condition = stack.pop().expect("checked by gate_fits");
                    let zero = self.constant(0, GATE_WIDTH);
                    let holds = self.binary(1, condition, BinaryOp::Ne, zero);
                    let value = self.b.alloc_bit(GATE_WIDTH, false);
                    self.b
                        .emit(SIRInstruction::Mux(value, holds, then, otherwise));
                    value
                }
                GateOp::And
                | GateOp::Or
                | GateOp::Xor
                | GateOp::Eq
                | GateOp::Ne
                | GateOp::LogicAnd
                | GateOp::LogicOr => {
                    let rhs = stack.pop().expect("checked by gate_fits");
                    let lhs = stack.pop().expect("checked by gate_fits");
                    match *op {
                        GateOp::And => self.binary(GATE_WIDTH, lhs, BinaryOp::And, rhs),
                        GateOp::Or => self.binary(GATE_WIDTH, lhs, BinaryOp::Or, rhs),
                        GateOp::Xor => self.binary(GATE_WIDTH, lhs, BinaryOp::Xor, rhs),
                        GateOp::Eq => {
                            let is = self.binary(1, lhs, BinaryOp::Eq, rhs);
                            self.widen(is)
                        }
                        GateOp::Ne => {
                            let is = self.binary(1, lhs, BinaryOp::Ne, rhs);
                            self.widen(is)
                        }
                        GateOp::LogicAnd => {
                            let is = self.binary(1, lhs, BinaryOp::LogicAnd, rhs);
                            self.widen(is)
                        }
                        _ => {
                            let is = self.binary(1, lhs, BinaryOp::LogicOr, rhs);
                            self.widen(is)
                        }
                    }
                }
            };
            stack.push(value);
        }
        let value = stack.pop().expect("checked by gate_fits");
        let one = self.constant(1, GATE_WIDTH);
        let low = self.binary(GATE_WIDTH, value, BinaryOp::And, one);
        let zero = self.constant(0, GATE_WIDTH);
        self.binary(1, low, BinaryOp::Ne, zero)
    }

    fn inline_all(&mut self, units: &[Unit]) {
        for unit in units {
            self.b.inline_execution_unit(unit);
        }
    }

    /// Settle combinational logic when the one-bit `condition` holds.
    fn settle_if(&mut self, condition: RegisterId, comb: &[Unit]) {
        let settle = self.b.new_block();
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (settle, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(settle);
        self.inline_all(comb);
        self.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
        self.b.switch_to_block(next);
    }

    /// Run `body` when bit `index` of `mask` is set.
    fn if_bit(&mut self, mask: RegisterId, index: usize, body: impl FnOnce(&mut Self)) {
        let selector = self.constant(1 << index, MAX_ENTRIES);
        let selected = self.binary(MAX_ENTRIES, mask, BinaryOp::And, selector);
        let zero = self.constant(0, MAX_ENTRIES);
        let set = self.binary(1, selected, BinaryOp::Ne, zero);
        let run = self.b.new_block();
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: set,
            true_block: (run, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(run);
        body(self);
        self.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
        self.b.switch_to_block(next);
    }
}

/// Where a generated edge unit learns whether combinational logic must
/// settle before the domains evaluate.
#[derive(Clone, Copy, Debug)]
enum Settle {
    #[cfg_attr(
        not(all(test, feature = "systemverilog", feature = "cranelift")),
        allow(dead_code)
    )]
    Fixed(bool),
    /// A flag object, not zero when it must.
    Flag(AbsoluteAddr),
}

impl EdgePlan {
    /// A unit that runs one edge of the event whose signal its caller
    /// already moved, rising or falling; `fires` says whether the event's own
    /// domain sees its active edge.
    fn unit(&self, comb: &[Unit], rising: bool, fires: bool, settle: Settle) -> Unit {
        let mut b = Builder::new();
        let constant = |b: &mut Builder, value: u64, width: usize| {
            let register = b.alloc_bit(width, false);
            b.emit(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        };
        let from = constant(&mut b, u64::from(!rising), 1);
        let to = constant(&mut b, u64::from(rising), 1);
        let source_fires = fires;
        let fires = constant(&mut b, u64::from(fires), 1);
        let settle = match settle {
            Settle::Fixed(value) => constant(&mut b, u64::from(value), 1),
            Settle::Flag(flag) => {
                let value = b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
                b.emit(SIRInstruction::Load(
                    value,
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, flag),
                    SIROffset::Static(0),
                    SCHEDULER_FLAG_WIDTH,
                ));
                let zero = constant(&mut b, 0, SCHEDULER_FLAG_WIDTH);
                let set = b.alloc_bit(1, false);
                b.emit(SIRInstruction::Binary(set, value, BinaryOp::Ne, zero));
                set
            }
        };
        let done = b.new_block();
        // An edge that never fires the event's own domain leaves its units
        // out of the unit.
        let plan = if source_fires {
            std::borrow::Cow::Borrowed(self)
        } else {
            std::borrow::Cow::Owned(EdgePlan {
                source: DomainUnits {
                    eval_apply: Vec::new(),
                    eval_only: None,
                    apply: None,
                },
                ..self.clone()
            })
        };
        plan.emit(&mut b, comb, from, to, fires, settle, done, done);
        b.switch_to_block(done);
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }
}

/// Generate the edge units of every event whose plan this module emits and
/// record them in [`celox_design::RuntimeSchema::derived_edges`]. A unit
/// reads the scheduler's dirty flag to know whether to settle first.
pub(crate) fn attach(program: &mut LaidOutProgram) {
    let schema = &program.runtime.runtime_schema;
    let Some(flag) = schema
        .scheduler
        .as_ref()
        .map(|scheduler| scheduler.slots.dirty)
    else {
        return;
    };
    let model = ScheduleModel::derive(&program.design, schema);
    let mut edges = Vec::new();
    for event in crate::edge_plans::event_addresses(program) {
        let Ok(Some(plan)) = EdgePlan::new(program, &model, event) else {
            continue;
        };
        // An edge of a clock or reset fires its domain when it is the active
        // one; any other moved two-state signal always does.
        let kind = model.kind(&event);
        let fires = |rising: bool| match kind {
            Some(
                DomainKind::ClockPosedge
                | DomainKind::ClockNegedge
                | DomainKind::ResetAsyncHigh
                | DomainKind::ResetAsyncLow,
            ) => rising == (active_level(kind) == 1),
            _ => true,
        };
        let mut indices = [0usize; 2];
        for (index, rising) in [true, false].into_iter().enumerate() {
            let unit = plan.unit(
                &program.sir.eval_comb,
                rising,
                fires(rising),
                Settle::Flag(flag),
            );
            indices[index] = program.sir.processes.len();
            program.sir.processes.push(unit);
        }
        edges.push(celox_design::DerivedEdgeUnits {
            event,
            rising: indices[0],
            falling: indices[1],
        });
    }
    program.runtime.runtime_schema.derived_edges = edges;
}

/// `program` without its derived-edge units, for a tier whose host walks
/// the derived-tick plans instead. The units follow every other process
/// unit, so the others keep their indices.
pub(crate) fn without_units(program: &LaidOutProgram) -> LaidOutProgram {
    let mut program = program.clone();
    let first = program
        .runtime
        .runtime_schema
        .derived_edges
        .iter()
        .map(|units| units.rising.min(units.falling))
        .min();
    if let Some(first) = first {
        program.sir.processes.truncate(first);
        program.runtime.runtime_schema.derived_edges.clear();
    }
    program
}

/// A generated edge unit and the edge it runs, for the tests.
#[cfg(all(test, feature = "systemverilog", feature = "cranelift"))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TestEdgeUnit {
    pub(crate) event: AbsoluteAddr,
    pub(crate) rising: bool,
    pub(crate) settle: bool,
    pub(crate) fires: bool,
    pub(crate) index: usize,
}

#[cfg(all(test, feature = "systemverilog", feature = "cranelift"))]
pub(crate) type TestEdgeUnits = std::sync::Arc<std::sync::Mutex<Vec<TestEdgeUnit>>>;

/// Append an edge unit of every event with a plan this module emits, for
/// each edge, settle and own-edge case, and record them in `sink`.
#[cfg(all(test, feature = "systemverilog", feature = "cranelift"))]
pub(crate) fn attach_test_units(program: &mut LaidOutProgram, sink: &TestEdgeUnits) {
    let model = ScheduleModel::derive(&program.design, &program.runtime.runtime_schema);
    for event in crate::edge_plans::event_addresses(program) {
        let Ok(Some(plan)) = EdgePlan::new(program, &model, event) else {
            continue;
        };
        for rising in [false, true] {
            for settle in [false, true] {
                for fires in [false, true] {
                    let unit =
                        plan.unit(&program.sir.eval_comb, rising, fires, Settle::Fixed(settle));
                    let index = program.sir.processes.len();
                    program.sir.processes.push(unit);
                    sink.lock().expect("unpoisoned").push(TestEdgeUnit {
                        event,
                        rising,
                        settle,
                        fires,
                        index,
                    });
                }
            }
        }
    }
}

#[cfg(all(test, feature = "systemverilog", feature = "cranelift"))]
mod tests {
    use std::path::Path;

    use celox_runtime::{
        BackendExecutor, DerivedTickScratch, Edge, EdgeOutcome, run_edge, set_level,
    };

    use super::TestEdgeUnits;
    use crate::{
        Simulator,
        backend::{EventHandle, SimBackend},
    };

    const GATED: &str = "module Top(input logic clk, input logic en, output logic [7:0] q);
                logic gclk;
                logic [7:0] r = 0;
                logic [7:0] g = 0;
                logic [7:0] w;
                assign gclk = clk & en;
                // A value the host's writes of `en` change through logic.
                assign w = en ? 8'd3 : 8'd1;
                always_ff @(posedge clk) r <= r + w;
                always_ff @(posedge gclk) g <= g + r;
                assign q = g;
            endmodule";

    const INVERTED: &str = "module Top(input logic clk, input logic en, output logic [7:0] q);
                logic nclk;
                logic gclk;
                logic [7:0] r = 0;
                logic [7:0] n = 0;
                logic [7:0] g = 0;
                assign nclk = ~clk;
                assign gclk = clk & en;
                always_ff @(posedge clk) r <= r + 8'd1;
                always_ff @(posedge nclk) n <= n + r;
                always_ff @(negedge gclk) g <= g ^ n;
                assign q = n + g;
            endmodule";

    const DIVIDERS: &str = "module Top(input logic clk, output logic [7:0] q);
                logic half = 0;
                logic quarter = 0;
                logic [7:0] h = 0;
                logic [7:0] w = 0;
                always_ff @(posedge clk) half <= ~half;
                always_ff @(posedge half) quarter <= ~quarter;
                always_ff @(posedge half) h <= h + 8'd1;
                always_ff @(posedge quarter) w <= w + h;
                assign q = h + w;
            endmodule";

    const RAISED_ENABLE: &str = "module Top(input logic clk, output logic [7:0] q);
                logic en = 0;
                logic gclk;
                logic [7:0] g = 0;
                assign gclk = clk & en;
                always_ff @(posedge clk) en <= ~en;
                always_ff @(posedge gclk) g <= g + 8'd1;
                assign q = g;
            endmodule";

    const INDEPENDENT: &str =
        "module Top(input logic clk, input logic en, input logic [7:0] d, output logic [7:0] q);
                logic gclk;
                logic [7:0] r = 0;
                logic [7:0] g = 0;
                assign gclk = clk & en;
                always_ff @(posedge clk) r <= r + d;
                always_ff @(posedge gclk) g <= g + 8'd3;
                assign q = r ^ g;
            endmodule";

    /// The design state: the stable region after its header.
    fn state<B: SimBackend>(backend: &B) -> Vec<u8> {
        let (ptr, len) = backend.memory_as_ptr();
        let size = backend.layout().total_size;
        assert!(size <= len);
        // Safety: the backend exposes `len` readable bytes at `ptr`.
        let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
        memory[celox_state_layout::STATE_HEADER_SIZE..size].to_vec()
    }

    /// Whether combinational logic is stale after an edge with `outcome`,
    /// as `run_tick` keeps it.
    fn stale(dirty: bool, outcome: EdgeOutcome) -> bool {
        match outcome {
            EdgeOutcome::Unchanged | EdgeOutcome::Stored => dirty,
            EdgeOutcome::Moved => true,
            EdgeOutcome::Settled => false,
        }
    }

    /// Run edges of `clk` and host writes of `en`, when the design has it,
    /// through the Rust plan walk and through the generated units, and
    /// compare the state after each.
    fn agrees(source: &str) {
        let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        // The Cranelift backend runs extra units without an image to check.
        let mut walked = build().build_cranelift().unwrap();
        let sink = TestEdgeUnits::default();
        let mut generated = build()
            .derived_edge_units(sink.clone())
            .build_cranelift()
            .unwrap();
        let units = sink.lock().unwrap().clone();
        assert!(!units.is_empty(), "no edge unit was generated");
        let clk = walked.event("clk");
        let address = walked.backend.id_to_addr_slice()[clk.id()];
        let plans = walked.derived_tick_plans();
        let plan = plans.get(clk.id()).and_then(Option::as_ref);
        assert!(plan.is_some(), "clk has no derived plan");
        let gated = plan.is_some_and(|plan| plan.lever_signals().next().is_some());
        let info = walked.event_edge(clk);
        let en = walked.try_signal("en").ok();
        let mut scratch = DerivedTickScratch::default();
        assert!(
            state(&generated.backend) == state(&walked.backend),
            "the simulators start in other states"
        );
        let (mut walked_dirty, mut generated_dirty) = (false, false);
        let edges = [
            Edge::Rising,
            Edge::Falling,
            Edge::Falling,
            Edge::Rising,
            Edge::Falling,
        ];
        for step in 0..60usize {
            if let Some(en) = en
                && step % 7 < 2
            {
                let value = u8::from(step % 3 != 0);
                walked.backend.set(en, value);
                generated.backend.set(en, value);
                walked_dirty = true;
                generated_dirty = true;
            }
            let edge = edges[step % edges.len()];
            let outcome = run_edge(
                &mut BackendExecutor(&mut walked.backend),
                plan,
                info,
                &mut scratch,
                clk,
                edge,
                walked_dirty,
                set_level,
            )
            .unwrap();
            walked_dirty = stale(walked_dirty, outcome);

            let from = u8::from(generated.backend.get_as::<u8>(info.signal) & 1 != 0);
            let outcome = if from == edge.level() {
                EdgeOutcome::Unchanged
            } else {
                generated.backend.set(info.signal, edge.level());
                let fires = info.fires(Some(from != 0), edge);
                let settle = generated_dirty || info.comb_read;
                let unit = units
                    .iter()
                    .find(|unit| {
                        unit.event == address
                            && unit.rising == (edge == Edge::Rising)
                            && unit.settle == settle
                            && unit.fires == fires
                    })
                    .expect("a unit for every case");
                generated.backend.run_process(unit.index).unwrap();
                if gated || fires {
                    EdgeOutcome::Settled
                } else if info.comb_read {
                    EdgeOutcome::Moved
                } else {
                    EdgeOutcome::Stored
                }
            };
            generated_dirty = stale(generated_dirty, outcome);
            assert_eq!(generated_dirty, walked_dirty, "step {step}");
            let (left, right) = (state(&generated.backend), state(&walked.backend));
            if let Some(offset) = left.iter().zip(&right).position(|(a, b)| a != b) {
                let offset = offset + celox_state_layout::STATE_HEADER_SIZE;
                let name = walked
                    .named_signals()
                    .into_iter()
                    .find(|named| {
                        let start = named.signal.offset;
                        (start..start + named.signal.width.div_ceil(8)).contains(&offset)
                    })
                    .map_or_else(|| "no named signal".to_string(), |named| named.name);
                panic!(
                    "step {step} ({edge:?}): the generated edge left state byte {offset} ({name}) otherwise"
                );
            }
        }
    }

    /// Drive `source` through the `Simulator` API, with derived domains run
    /// by the generated edge units and by the plan walk, and compare every
    /// named signal after each operation: ticks of `clk`, single and
    /// repeated edges, and host writes of `en` when the design has it.
    fn ticks_agree(source: &str) {
        let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        let mut generated = build().build().unwrap();
        let mut walked = build()
            .disable_pass(crate::SirPass::DerivedEdgeUnits)
            .build()
            .unwrap();
        assert!(
            !generated.program.runtime_schema.derived_edges.is_empty(),
            "no edge unit was generated"
        );
        assert!(walked.program.runtime_schema.derived_edges.is_empty());
        let clk = generated.event("clk");
        let en = generated.try_signal("en").ok();
        let names: Vec<_> = walked.named_signals();
        for step in 0..80usize {
            for sim in [&mut generated, &mut walked] {
                if let Some(en) = en
                    && step % 7 < 2
                {
                    sim.modify(|io| io.set(en, u8::from(step % 3 != 0)))
                        .unwrap();
                }
                match step % 5 {
                    0 | 1 => sim.tick(clk).unwrap(),
                    2 => sim.edge(clk, Edge::Rising).unwrap(),
                    3 => sim.edge(clk, Edge::Rising).unwrap(),
                    _ => sim.edge(clk, Edge::Falling).unwrap(),
                }
            }
            for named in &names {
                assert_eq!(
                    generated.get(named.signal),
                    walked.get(named.signal),
                    "step {step}: {}",
                    named.name
                );
            }
        }
    }

    #[test]
    fn simulator_ticks_run_generated_edges_as_the_plan_walk() {
        for source in [GATED, INVERTED, DIVIDERS, RAISED_ENABLE, INDEPENDENT] {
            ticks_agree(source);
        }
    }

    /// An edge reaching 60 gated domains.
    #[test]
    fn an_edge_reaching_60_gated_domains_runs_as_the_plan_walk() {
        const GATED_CLOCKS: usize = 60;
        let mut source = String::from(
            "module Top(input logic clk, input logic en, output logic [7:0] q);\n  \
             logic [6:0] count = 0;\n  logic [7:0] sum;\n  \
             always_ff @(posedge clk) count <= count + 7'd1;\n",
        );
        let mut terms = Vec::new();
        for index in 0..GATED_CLOCKS {
            source += &format!(
                "  logic g{index};\n  logic [7:0] n{index} = 0;\n  \
                 assign g{index} = clk & (en ^ (count[2:0] == 3'd{bits}) ^ (count[6:3] == 4'd{high}));\n  \
                 always_ff @(posedge g{index}) n{index} <= n{index} + 8'd1;\n",
                bits = index % 8,
                high = index / 8,
            );
            terms.push(format!("n{index}"));
        }
        source += &format!("  assign q = {};\nendmodule\n", terms.join(" ^ "));
        ticks_agree(&source);
    }

    #[test]
    fn gated_clocks_run_as_the_plan_walk() {
        agrees(GATED);
    }

    #[test]
    fn inverted_and_falling_gated_clocks_run_as_the_plan_walk() {
        agrees(INVERTED);
    }

    #[test]
    fn dividers_run_as_the_plan_walk() {
        agrees(DIVIDERS);
    }

    #[test]
    fn enables_the_commit_raises_run_as_the_plan_walk() {
        agrees(RAISED_ENABLE);
    }

    #[test]
    fn independent_gated_domains_run_as_the_plan_walk() {
        agrees(INDEPENDENT);
    }
}
