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
use num_bigint::BigUint;

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

/// The narrowest fire mask, one bit per domain an edge can fire: one 64-bit
/// word, which every edge reaching at most 64 domains uses. An edge reaching
/// more uses as many words as its domains need.
const MIN_ENTRY_BITS: usize = 64;

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
                .any(crate::ir::may_raise_fatal_capture)
        });
        if comb_fatal {
            return Err("combinational logic has fatal assertions");
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

    /// The width of a fire mask: one bit per entry, at least
    /// [`MIN_ENTRY_BITS`] and a whole number of words.
    fn entry_bits(&self) -> usize {
        (1 + self.domains.len() + self.gated.len())
            .next_multiple_of(64)
            .max(MIN_ENTRY_BITS)
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
        let bits = self.entry_bits();
        let mut e = Emitter {
            b,
            after: Vec::new(),
            entry_bits: bits,
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
        let mask = e.b.alloc_bit(bits, false);
        let fired = e.b.alloc_bit(bits, false);
        let settle_first = e.b.alloc_bit(1, false);
        let alone = e.b.alloc_bit(1, false);
        let fire = e.b.new_block_with(vec![mask, fired, settle_first, alone]);
        let source = e.bit(fires, 0);
        if self.gated.is_empty() {
            let zero = e.constant(0, bits);
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
            let mut gated_fired = e.constant(0, bits);
            let mut after = Vec::with_capacity(self.gated.len());
            for (index, gated) in self.gated.iter().enumerate() {
                let before = e.gate(&gated.gate, from);
                let level = e.gate(&gated.gate, to);
                let edge = e.edge(gated.derived.kind, before, level);
                let bit = e.bit(edge, self.gated_bit(index));
                ready = e.binary(bits, ready, BinaryOp::Or, bit);
                gated_fired = e.binary(bits, gated_fired, BinaryOp::Or, bit);
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
        let zero = e.constant(0, bits);
        let gated_mask = self
            .gated
            .iter()
            .enumerate()
            .fold(BigUint::ZERO, |mask, (index, _)| {
                mask | BigUint::from(1u8) << self.gated_bit(index)
            });
        let gated_bits = e.wide_constant(gated_mask, bits);
        let gated_ready = e.binary(bits, mask, BinaryOp::And, gated_bits);
        let any_gated = e.binary(1, gated_ready, BinaryOp::Ne, zero);
        let in_turn = e.binary(1, alone, BinaryOp::And, any_gated);
        // Each domain's units appear once in the unit: the one-domain path
        // also serves the independent gated domains one at a time, and only
        // domains that can fire with another get the several-domain path.
        let one_mask = e.b.alloc_bit(bits, false);
        let remaining = e.b.alloc_bit(bits, false);
        let turn = e.b.alloc_bit(1, false);
        let one_domain = e.b.new_block_with(vec![one_mask, remaining, turn]);
        let rest = e.b.alloc_bit(bits, false);
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
            let bit = e.single(entry, bits);
            let selected = e.binary(bits, rest, BinaryOp::And, bit);
            let set = e.binary(1, selected, BinaryOp::Ne, zero);
            let take = e.b.new_block();
            let skip = e.b.new_block();
            e.b.seal_block(SIRTerminator::Branch {
                cond: set,
                true_block: (take, Vec::new()),
                false_block: (skip, Vec::new()),
            });
            e.b.switch_to_block(take);
            let all = e.ones(bits);
            let others = e.binary(bits, bit, BinaryOp::Xor, all);
            let left = e.binary(bits, rest, BinaryOp::And, others);
            let yes = e.constant(1, 1);
            e.b.seal_block(SIRTerminator::Jump(one_domain, vec![bit, left, yes]));
            e.b.switch_to_block(skip);
        }
        e.b.seal_block(SIRTerminator::Jump(fire_end, Vec::new()));

        // The domains together: one evaluates and commits at once, several
        // evaluate, then commit, in entry order.
        e.b.switch_to_block(together);
        let one = e.constant(1, bits);
        let lower = e.binary(bits, mask, BinaryOp::Sub, one);
        let others = e.binary(bits, mask, BinaryOp::And, lower);
        let single = e.binary(1, others, BinaryOp::Eq, zero);
        let several = e.b.new_block();
        let no = e.constant(0, 1);
        let none_left = e.constant(0, bits);
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
        let mut ready = e.constant(0, bits);
        for (index, domain) in self.domains.iter().enumerate() {
            let is = e.level(domain.signal, domain.width);
            let edge = e.edge(domain.kind, was[index], is);
            let bit = e.bit(edge, 1 + index);
            ready = e.binary(bits, ready, BinaryOp::Or, bit);
        }
        let after = std::mem::take(&mut e.after);
        for (index, gated) in self.gated.iter().enumerate() {
            let is = e.gate(&gated.gate, to);
            let edge = e.edge(gated.derived.kind, after[index], is);
            let bit = e.bit(edge, self.gated_bit(index));
            ready = e.binary(bits, ready, BinaryOp::Or, bit);
        }
        let all = e.ones(bits);
        let unfired = e.binary(bits, fired, BinaryOp::Xor, all);
        let ready = e.binary(bits, ready, BinaryOp::And, unfired);
        let quiet = e.binary(1, ready, BinaryOp::Eq, zero);
        let round = e.b.new_block();
        e.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (settled, Vec::new()),
            false_block: (round, Vec::new()),
        });
        e.b.switch_to_block(round);
        let now_fired = e.binary(bits, fired, BinaryOp::Or, ready);
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
    /// See [`EdgePlan::entry_bits`].
    entry_bits: usize,
}

impl Emitter<'_> {
    fn constant(&mut self, value: u64, width: usize) -> RegisterId {
        self.wide_constant(BigUint::from(value), width)
    }

    fn wide_constant(&mut self, value: BigUint, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    /// The `width`-bit value with only bit `bit` set.
    fn single(&mut self, bit: usize, width: usize) -> RegisterId {
        self.wide_constant(BigUint::from(1u8) << bit, width)
    }

    /// The `width`-bit value with every bit set.
    fn ones(&mut self, width: usize) -> RegisterId {
        self.wide_constant((BigUint::from(1u8) << width) - 1u8, width)
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
        let set = self.single(index, self.entry_bits);
        let unset = self.constant(0, self.entry_bits);
        let bit = self.b.alloc_bit(self.entry_bits, false);
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
        let selector = self.single(index, self.entry_bits);
        let selected = self.binary(self.entry_bits, mask, BinaryOp::And, selector);
        let zero = self.constant(0, self.entry_bits);
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
    #[cfg_attr(not(all(test, feature = "cranelift")), allow(dead_code))]
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
///
/// The units run edges the host moves. Without `host_edges` (a timed
/// simulation, which has no such API), only the events a kernel the compiled
/// scheduler does not run writes get units: the host runs those kernels and
/// moves what they write; the scheduler moves every other event signal in
/// its slots, and a slot it hands back walks the plans.
pub(crate) fn attach(program: &mut LaidOutProgram, host_edges: bool) {
    attach_with_mode(program, false, host_edges);
}

/// Reserve native entry indices without retaining a copy of the full circuit
/// for both polarities of every derived-clock event.
pub(crate) fn attach_deferred(program: &mut LaidOutProgram, host_edges: bool) {
    attach_with_mode(program, true, host_edges);
}

/// The event signals the host may move in a timed simulation: those a
/// kernel the compiled scheduler does not run stores, or `None` when no
/// compiled scheduler runs the slots and the host moves every one.
fn host_moved_events(program: &LaidOutProgram) -> Option<crate::HashSet<AbsoluteAddr>> {
    let scheduler = program.runtime.runtime_schema.scheduler.as_ref()?;
    scheduler.unit?;
    let mut written = crate::HashSet::default();
    for (index, reason) in scheduler.fallback.iter().enumerate() {
        if reason.is_none() {
            continue;
        }
        let Some(unit) = program.sir.processes.get(index) else {
            continue;
        };
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            match instruction {
                SIRInstruction::Store(address, ..) | SIRInstruction::Commit(_, address, ..) => {
                    written.insert(address.absolute_addr());
                }
                _ => {}
            }
            if let Some(effects) = instruction.call_effects() {
                written.extend(effects.writes.iter().map(|address| address.absolute_addr()));
            }
        }
    }
    Some(written)
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredEdge {
    event: AbsoluteAddr,
    rising: bool,
    fires: bool,
    flag: AbsoluteAddr,
    pub(crate) blocks: usize,
    pub(crate) instructions: usize,
}

impl DeferredEdge {
    pub(crate) fn generate(
        &self,
        program: &LaidOutProgram,
        model: &ScheduleModel<AbsoluteAddr>,
    ) -> Result<Unit, &'static str> {
        self.generate_with_comb(program, model, &program.sir.eval_comb)
    }

    pub(crate) fn generate_with_comb(
        &self,
        program: &LaidOutProgram,
        model: &ScheduleModel<AbsoluteAddr>,
        comb: &[Unit],
    ) -> Result<Unit, &'static str> {
        let plan = EdgePlan::new(program, model, self.event)?
            .ok_or("deferred derived edge no longer has a plan")?;
        let mut unit = plan.unit(comb, self.rising, self.fires, Settle::Flag(self.flag));
        crate::settle::instrument_unit(program, &mut unit);
        Ok(unit)
    }
}

fn attach_with_mode(program: &mut LaidOutProgram, deferred: bool, host_edges: bool) {
    let schema = &program.runtime.runtime_schema;
    let Some(flag) = schema
        .scheduler
        .as_ref()
        .map(|scheduler| scheduler.slots.dirty)
    else {
        return;
    };
    let host_moved = if host_edges {
        None
    } else {
        host_moved_events(program)
    };
    let model = ScheduleModel::derive(&program.design, schema);
    let mut edges = Vec::new();
    for event in crate::edge_plans::event_addresses(program) {
        if host_moved
            .as_ref()
            .is_some_and(|host_moved| !host_moved.contains(&event))
        {
            continue;
        }
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
        // Include both settles and every possible FF path. The control-flow
        // allowance also covers gate expressions and per-unit continuations.
        let (estimated_blocks, estimated_instructions) = if deferred {
            let units = program
                .sir
                .eval_comb
                .iter()
                .chain(&program.sir.eval_comb)
                .chain(plan.entries().flat_map(|units| {
                    units
                        .eval_apply
                        .iter()
                        .chain(units.eval_only.as_ref().unwrap_or(&units.eval_apply))
                        .chain(units.apply.iter().flatten())
                }));
            let gate_ops = plan.gated.iter().map(|gate| gate.gate.len()).sum::<usize>();
            units.fold(
                (
                    1024usize,
                    8192usize.saturating_add(gate_ops.saturating_mul(16)),
                ),
                |(blocks, instructions), unit| {
                    (
                        blocks.saturating_add(unit.blocks.len()).saturating_add(2),
                        instructions.saturating_add(
                            unit.blocks
                                .values()
                                .map(|b| b.instructions.len())
                                .sum::<usize>(),
                        ),
                    )
                },
            )
        } else {
            (0, 0)
        };
        if deferred {
            tracing::debug!(
                "[derived-edge-size] event={} kind={:?} blocks={} instructions={}",
                program.get_path(&event),
                kind,
                estimated_blocks,
                estimated_instructions,
            );
        }
        for (index, rising) in [true, false].into_iter().enumerate() {
            indices[index] = program.sir.processes.len();
            let unit = if deferred {
                program.deferred_derived_edges.insert(
                    indices[index],
                    DeferredEdge {
                        event,
                        rising,
                        fires: fires(rising),
                        flag,
                        blocks: estimated_blocks,
                        instructions: estimated_instructions,
                    },
                );
                let mut builder = Builder::new();
                builder.seal_block(SIRTerminator::Return);
                let (blocks, register_map, _) = builder.drain();
                Unit {
                    entry_block_id: BlockId(0),
                    blocks,
                    register_map,
                }
            } else {
                plan.unit(
                    &program.sir.eval_comb,
                    rising,
                    fires(rising),
                    Settle::Flag(flag),
                )
            };
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
        program.deferred_derived_edges.clear();
        program.runtime.runtime_schema.derived_edges.clear();
    }
    program
}

/// A generated edge unit and the edge it runs, for the tests.
#[cfg(all(test, feature = "cranelift"))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TestEdgeUnit {
    pub(crate) event: AbsoluteAddr,
    pub(crate) rising: bool,
    pub(crate) settle: bool,
    pub(crate) fires: bool,
    pub(crate) index: usize,
}

#[cfg(all(test, feature = "cranelift"))]
#[allow(
    clippy::disallowed_types,
    reason = "test-only: the tests read back the edge units that the compiled program records"
)]
pub(crate) type TestEdgeUnits = std::sync::Arc<std::sync::Mutex<Vec<TestEdgeUnit>>>;

/// Append an edge unit of every event with a plan this module emits, for
/// each edge, settle and own-edge case, and record them in `sink`.
#[cfg(all(test, feature = "cranelift"))]
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

#[cfg(all(test, feature = "cranelift"))]
mod tests;
