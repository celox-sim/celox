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
    DERIVED_EDGE_FALLING, DERIVED_EDGE_MOVED, DERIVED_EDGE_RISING, DomainKind, GateOp,
    SCHEDULER_FLAG_WIDTH, STABLE_REGION, edges::active_level, schedule::ScheduleModel,
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

    /// Whether `bit` is set in the mode `mode`.
    fn mode_bit(&mut self, mode: RegisterId, bit: u8) -> RegisterId {
        let selector = self.constant(u64::from(bit), SCHEDULER_FLAG_WIDTH);
        let selected = self.binary(SCHEDULER_FLAG_WIDTH, mode, BinaryOp::And, selector);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        self.binary(1, selected, BinaryOp::Ne, zero)
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

impl EdgePlan {
    /// A unit that runs one edge of the event whose signal its caller
    /// already moved, rising or falling; `fires` says whether the event's own
    /// domain sees its active edge, `settle` whether logic settles first.
    #[cfg(all(test, feature = "systemverilog", feature = "cranelift"))]
    fn unit(&self, comb: &[Unit], rising: bool, fires: bool, settle: bool) -> Unit {
        let mut b = Builder::new();
        let constant = |b: &mut Builder, value: u64, width: usize| {
            let register = b.alloc_bit(width, false);
            b.emit(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        };
        let from = constant(&mut b, u64::from(!rising), 1);
        let to = constant(&mut b, u64::from(rising), 1);
        let fired = constant(&mut b, u64::from(fires), 1);
        let settle = constant(&mut b, u64::from(settle), 1);
        let done = b.new_block();
        self.for_edge(fires)
            .emit(&mut b, comb, from, to, fired, settle, done, done);
        b.switch_to_block(done);
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    /// The plan of an edge that `fires` the event's own domain or not: one
    /// that never does leaves its units out.
    fn for_edge(&self, fires: bool) -> std::borrow::Cow<'_, Self> {
        if fires {
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
        }
    }

    /// The unit of [`celox_design::DerivedEdgeUnits`]: the edges of `event`
    /// (a signal of `width` bits) that the mode in `slots.0` selects, with
    /// `slots.1` the flag that says whether combinational logic has pending
    /// changes, before and after, as the host keeps it: an edge settles first
    /// when it is set or logic reads the signal (`comb_read`) that the edge
    /// moved itself; an edge that ran domains leaves logic settled; one that
    /// ran nothing leaves it as it found it, pending when logic reads the
    /// signal the edge moved. `fires` says whether the rising
    /// (`true`) or the falling edge fires the event's own domain.
    fn edges_unit(
        &self,
        comb: &[Unit],
        event: (AbsoluteAddr, usize),
        comb_read: bool,
        fires: impl Fn(bool) -> bool,
        slots: (AbsoluteAddr, AbsoluteAddr),
    ) -> Unit {
        let (signal, width) = event;
        let (mode_slot, flag) = slots;
        let mut b = Builder::new();
        let mut e = Emitter {
            b: &mut b,
            after: Vec::new(),
            entry_bits: self.entry_bits(),
        };
        let mode = e.load(mode_slot, 0, SCHEDULER_FLAG_WIDTH);
        let flag_value = e.load(flag, 0, SCHEDULER_FLAG_WIDTH);
        let zero = e.constant(0, SCHEDULER_FLAG_WIDTH);
        let mut dirty = e.binary(1, flag_value, BinaryOp::Ne, zero);
        for rising in [true, false] {
            let selected = e.mode_bit(
                mode,
                if rising {
                    DERIVED_EDGE_RISING
                } else {
                    DERIVED_EDGE_FALLING
                },
            );
            let run = e.b.new_block();
            let after = e.b.alloc_bit(1, false);
            let join = e.b.new_block_with(vec![after]);
            e.b.seal_block(SIRTerminator::Branch {
                cond: selected,
                true_block: (run, Vec::new()),
                false_block: (join, vec![dirty]),
            });
            // The host moved the signal for a single edge; otherwise the
            // edge moves it, unless it is already at the edge's level.
            e.b.switch_to_block(run);
            let moved = e.mode_bit(mode, DERIVED_EDGE_MOVED);
            // A host that moved the signal judged whether logic reading it
            // must settle (it may have evaluated the clock's aliases) and
            // put that in the flag.
            let read = if comb_read {
                let no = e.constant(0, 1);
                e.binary(1, moved, BinaryOp::Eq, no)
            } else {
                e.constant(0, 1)
            };
            let body = e.b.new_block();
            let check = e.b.new_block();
            e.b.seal_block(SIRTerminator::Branch {
                cond: moved,
                true_block: (body, Vec::new()),
                false_block: (check, Vec::new()),
            });
            e.b.switch_to_block(check);
            let level = e.level(signal, width);
            let target = e.constant(u64::from(rising), 1);
            let there = e.binary(1, level, BinaryOp::Eq, target);
            let store = e.b.new_block();
            e.b.seal_block(SIRTerminator::Branch {
                cond: there,
                true_block: (join, vec![dirty]),
                false_block: (store, Vec::new()),
            });
            e.b.switch_to_block(store);
            let value = e.constant(u64::from(rising), width);
            e.b.emit(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, signal),
                SIROffset::Static(0),
                width,
                value,
                Vec::new(),
                Vec::new(),
            ));
            e.b.seal_block(SIRTerminator::Jump(body, Vec::new()));

            e.b.switch_to_block(body);
            let settle = e.binary(1, dirty, BinaryOp::Or, read);
            let from = e.constant(u64::from(!rising), 1);
            let to = e.constant(u64::from(rising), 1);
            let source_fires = fires(rising);
            let fired = e.constant(u64::from(source_fires), 1);
            let settled = e.b.new_block();
            let unsettled = e.b.new_block();
            self.for_edge(source_fires)
                .emit(e.b, comb, from, to, fired, settle, settled, unsettled);
            e.b.switch_to_block(settled);
            let clean = e.constant(0, 1);
            e.b.seal_block(SIRTerminator::Jump(join, vec![clean]));
            e.b.switch_to_block(unsettled);
            e.b.seal_block(SIRTerminator::Jump(join, vec![settle]));
            e.b.switch_to_block(join);
            dirty = after;
        }
        let set = e.constant(1, SCHEDULER_FLAG_WIDTH);
        let clear = e.constant(0, SCHEDULER_FLAG_WIDTH);
        let value = e.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
        e.b.emit(SIRInstruction::Mux(value, dirty, set, clear));
        e.b.emit(SIRInstruction::Store(
            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, flag),
            SIROffset::Static(0),
            SCHEDULER_FLAG_WIDTH,
            value,
            Vec::new(),
            Vec::new(),
        ));
        e.b.seal_block(SIRTerminator::Return);
        let (mut blocks, register_map, _) = b.drain();
        // An edge whose domains always run never takes its unsettled exit.
        let mut reached = crate::HashSet::default();
        let mut pending = vec![BlockId(0)];
        while let Some(block) = pending.pop() {
            if !reached.insert(block) {
                continue;
            }
            match &blocks[&block].terminator {
                SIRTerminator::Jump(target, _) => pending.push(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => pending.extend([true_block.0, false_block.0]),
                SIRTerminator::Switch { cases, default, .. } => {
                    pending.extend(cases.iter().map(|case| case.target));
                    pending.push(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        blocks.retain(|block, _| reached.contains(block));
        Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }
}

/// Generate the edge unit of every event whose plan this module emits and
/// record them in [`celox_design::RuntimeSchema::derived_edges`]. A unit
/// reads its mode from the scheduler's continuation slot and keeps the
/// state of combinational logic in its dirty flag, which only the
/// scheduler's own unit uses otherwise.
pub(crate) fn attach(program: &mut LaidOutProgram) {
    attach_with_mode(program, false);
}

/// Reserve native entry indices without retaining a copy of the full circuit
/// for the unit of every derived-clock event.
pub(crate) fn attach_deferred(program: &mut LaidOutProgram) {
    attach_with_mode(program, true);
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredEdge {
    event: AbsoluteAddr,
    width: usize,
    comb_read: bool,
    /// Whether the rising and the falling edge fire the event's own domain.
    fires: [bool; 2],
    /// The mode slot and the dirty flag.
    slots: (AbsoluteAddr, AbsoluteAddr),
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
        let mut unit = plan.edges_unit(
            comb,
            (self.event, self.width),
            self.comb_read,
            |rising| self.fires[usize::from(!rising)],
            self.slots,
        );
        crate::settle::instrument_unit(program, &mut unit);
        Ok(unit)
    }
}

fn attach_with_mode(program: &mut LaidOutProgram, deferred: bool) {
    let schema = &program.runtime.runtime_schema;
    let Some((mode, flag)) = schema
        .scheduler
        .as_ref()
        .map(|scheduler| (scheduler.slots.cont, scheduler.slots.dirty))
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
        let Some(&width) = program.layout().widths.get(&event) else {
            continue;
        };
        let comb_read = model.events.combinationally_read.contains(&event);
        let fires = [fires(true), fires(false)];
        let index = program.sir.processes.len();
        let unit = if deferred {
            // Include both settles of both edges and every possible FF path
            // of each. The control-flow allowance also covers gate
            // expressions, the mode checks and per-unit continuations.
            let units = std::iter::repeat_n(&program.sir.eval_comb, 4)
                .flatten()
                .chain(
                    std::iter::repeat_n((), 2)
                        .flat_map(|()| plan.entries())
                        .flat_map(|units| {
                            units
                                .eval_apply
                                .iter()
                                .chain(units.eval_only.as_ref().unwrap_or(&units.eval_apply))
                                .chain(units.apply.iter().flatten())
                        }),
                );
            let gate_ops = plan.gated.iter().map(|gate| gate.gate.len()).sum::<usize>();
            let (blocks, instructions) = units.fold(
                (
                    2048usize,
                    16384usize.saturating_add(gate_ops.saturating_mul(32)),
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
            );
            tracing::debug!(
                "[derived-edge-size] event={} kind={:?} blocks={} instructions={}",
                program.get_path(&event),
                kind,
                blocks,
                instructions,
            );
            program.deferred_derived_edges.insert(
                index,
                DeferredEdge {
                    event,
                    width,
                    comb_read,
                    fires,
                    slots: (mode, flag),
                    blocks,
                    instructions,
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
            plan.edges_unit(
                &program.sir.eval_comb,
                (event, width),
                comb_read,
                |rising| fires[usize::from(!rising)],
                (mode, flag),
            )
        };
        edges.push(celox_design::DerivedEdgeUnits { event, unit: index });
        program.sir.processes.push(unit);
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
        .map(|units| units.unit)
        .min();
    if let Some(first) = first {
        program.sir.processes.truncate(first);
        program.deferred_derived_edges.clear();
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
                    let unit = plan.unit(&program.sir.eval_comb, rising, fires, settle);
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

    /// Edges of `clk` that run nothing leave a host write of `en` for the
    /// next edge to settle: logic does not read `clk`.
    const DIVIDED_ENABLE: &str =
        "module Top(input logic clk, input logic en, output logic [7:0] q);
                logic half = 0;
                logic [7:0] h = 0;
                logic [7:0] w;
                assign w = en ? 8'd3 : 8'd1;
                always_ff @(posedge clk) half <= ~half;
                always_ff @(posedge half) h <= h + w;
                assign q = h;
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
            // The settle's partition flags hold only what is left to
            // recompute.
            let flags = walked
                .backend
                .layout()
                .settle
                .as_ref()
                .map(|settle| settle.flags());
            if let Some(offset) = left
                .iter()
                .zip(&right)
                .enumerate()
                .map(|(offset, pair)| (offset + celox_state_layout::STATE_HEADER_SIZE, pair))
                .find(|(offset, (a, b))| {
                    a != b && !flags.as_ref().is_some_and(|flags| flags.contains(offset))
                })
                .map(|(offset, _)| offset)
            {
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
        ticks_agree_on(
            build().build().unwrap(),
            build()
                .disable_pass(crate::SirPass::DerivedEdgeUnits)
                .build()
                .unwrap(),
        );
    }

    /// [`ticks_agree`] for two simulators of one backend.
    fn ticks_agree_on<B: SimBackend>(mut generated: Simulator<B>, mut walked: Simulator<B>) {
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

    /// Run batches of periods of `clk` in the generated unit and as edges
    /// through the plan walk, with host writes of `en` and single rising
    /// edges in between, and compare every named signal after each batch.
    fn periods_agree(source: &str) {
        let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        let mut generated = build().build().unwrap();
        let mut walked = build()
            .disable_pass(crate::SirPass::DerivedEdgeUnits)
            .build()
            .unwrap();
        let clk = generated.event("clk");
        let en = generated.try_signal("en").ok();
        let names: Vec<_> = walked.named_signals();
        let mut longest = 0;
        for step in 0..40u64 {
            for sim in [&mut generated, &mut walked] {
                if let Some(en) = en
                    && step % 5 < 2
                {
                    sim.modify(|io| io.set(en, u8::from(step % 3 != 0)))
                        .unwrap();
                }
            }
            // A batch may start with the signal high: its first rise moves
            // nothing.
            if step % 4 == 3 {
                generated.edge(clk, Edge::Rising).unwrap();
                walked.edge(clk, Edge::Rising).unwrap();
            }
            let count = step % 7 + 1;
            let mut left = count;
            while left > 0 {
                let (ran, result) = generated
                    .derived_periods(clk, left)
                    .expect("clk runs its generated unit");
                result.unwrap();
                assert!(ran >= 1 && ran <= left, "step {step}: {ran} of {left}");
                longest = longest.max(ran);
                left -= ran;
            }
            for _ in 0..count {
                walked.edge(clk, Edge::Rising).unwrap();
                walked.edge(clk, Edge::Falling).unwrap();
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
        // The native unit loops over the periods of a batch.
        assert!(longest > 1, "no batch ran more than one period");
    }

    #[test]
    fn periods_run_in_generated_units_as_the_plan_walk() {
        for source in [
            GATED,
            INVERTED,
            DIVIDERS,
            DIVIDED_ENABLE,
            RAISED_ENABLE,
            INDEPENDENT,
        ] {
            periods_agree(source);
        }
    }

    /// A batch of periods returns after the period whose derived domain
    /// records a runtime event, which the host sees in order.
    #[test]
    fn periods_return_after_a_runtime_event() {
        let source = "module Top(input logic clk, output logic [7:0] q);
                logic half = 0;
                logic [7:0] h = 0;
                always_ff @(posedge clk) half <= ~half;
                always_ff @(posedge half) begin
                    h <= h + 8'd1;
                    $display(\"h=%0d\", h);
                end
                assign q = h;
            endmodule";
        let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        let mut generated = build().build().unwrap();
        let mut walked = build()
            .disable_pass(crate::SirPass::DerivedEdgeUnits)
            .build()
            .unwrap();
        let clk = generated.event("clk");
        let mut displays = Vec::new();
        let mut left = 9u64;
        while left > 0 {
            let (ran, result) = generated.derived_periods(clk, left).unwrap();
            result.unwrap();
            // A display every other period: a batch ends at one.
            assert!(ran <= 2, "{ran} periods ran past a display");
            displays.extend(generated.drain_runtime_events());
            left -= ran;
        }
        let mut expected = Vec::new();
        for _ in 0..9 {
            walked.edge(clk, Edge::Rising).unwrap();
            walked.edge(clk, Edge::Falling).unwrap();
            expected.extend(walked.drain_runtime_events());
        }
        assert_eq!(displays, expected);
        assert_eq!(expected.len(), 5);
    }

    /// How a host drives the periods of `clk`.
    #[derive(Clone, Copy, Debug)]
    enum Drive {
        Ticks,
        Edges,
        Periods,
    }

    /// Drive up to `limit` periods of `clk` through `sim` and return the
    /// period that failed and its error, if one did.
    fn first_failure<B: SimBackend>(
        sim: &mut Simulator<B>,
        drive: Drive,
        limit: u64,
    ) -> Option<(u64, String)> {
        let clk = sim.event("clk");
        let period_edges = |sim: &mut Simulator<B>| {
            sim.edge(clk, Edge::Rising)
                .and_then(|()| sim.edge(clk, Edge::Falling))
        };
        let mut period = 0;
        while period < limit {
            // The periods a call ran, the one that failed included.
            let (ran, result) = match drive {
                Drive::Ticks => (1, sim.tick(clk)),
                Drive::Edges => (1, period_edges(sim)),
                Drive::Periods => sim
                    .derived_periods(clk, limit - period)
                    // The plan walk has no batches.
                    .unwrap_or_else(|| (1, period_edges(sim))),
            };
            if let Err(error) = result {
                return Some((period + ran.saturating_sub(1), error.to_string()));
            }
            period += ran;
        }
        None
    }

    /// A deferred assertion's action in a derived domain matures at the end
    /// of the edge, the tick or the period that ran it, as in the plan walk.
    #[test]
    fn deferred_assertions_of_derived_domains_mature() {
        let source = "module Top(input logic clk, output logic [7:0] q);
                logic half = 0;
                logic [7:0] h = 0;
                always_ff @(posedge clk) half <= ~half;
                always_ff @(posedge half) begin
                    h <= h + 8'd1;
                    assert #0 (h != 8'd3) else $fatal(1, \"h reached 3\");
                end
                assign q = h;
            endmodule";
        let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        for drive in [Drive::Ticks, Drive::Edges, Drive::Periods] {
            let expected = first_failure(
                &mut build()
                    .disable_pass(crate::SirPass::DerivedEdgeUnits)
                    .build()
                    .unwrap(),
                drive,
                20,
            );
            assert!(expected.is_some(), "{drive:?}: the walk reports no failure");
            let native = first_failure(&mut build().build().unwrap(), drive, 20);
            assert_eq!(native, expected, "{drive:?} native");
            let cranelift = first_failure(&mut build().build_cranelift().unwrap(), drive, 20);
            assert_eq!(cranelift, expected, "{drive:?} cranelift");
        }
    }

    /// A batch that returned after a runtime event leaves no periods for a
    /// single edge afterwards to run.
    #[test]
    fn single_edges_after_a_batch_returned_early_run_once() {
        let source = "module Top(input logic clk, output logic [7:0] q);
                logic half = 0;
                logic [7:0] h = 0;
                always_ff @(posedge clk) half <= ~half;
                always_ff @(posedge half) begin
                    h <= h + 8'd1;
                    $display(\"h=%0d\", h);
                end
                assign q = h;
            endmodule";
        let build = || Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        let mut generated = build().build().unwrap();
        let mut walked = build()
            .disable_pass(crate::SirPass::DerivedEdgeUnits)
            .build()
            .unwrap();
        let clk = generated.event("clk");
        let (ran, result) = generated.derived_periods(clk, 9).unwrap();
        result.unwrap();
        assert!(ran < 9, "the batch did not return at its display");
        for _ in 0..ran {
            walked.edge(clk, Edge::Rising).unwrap();
            walked.edge(clk, Edge::Falling).unwrap();
        }
        let q = generated.signal("q");
        for step in 0..6 {
            match step % 3 {
                0 => {
                    generated.tick(clk).unwrap();
                    walked.tick(clk).unwrap();
                }
                1 => {
                    generated.edge(clk, Edge::Rising).unwrap();
                    walked.edge(clk, Edge::Rising).unwrap();
                }
                _ => {
                    generated.edge(clk, Edge::Falling).unwrap();
                    walked.edge(clk, Edge::Falling).unwrap();
                }
            }
            assert_eq!(generated.get(q), walked.get(q), "step {step}");
        }
        assert_eq!(
            generated.drain_runtime_events(),
            walked.drain_runtime_events()
        );
    }

    #[test]
    fn simulator_ticks_run_generated_edges_as_the_plan_walk() {
        for source in [
            GATED,
            INVERTED,
            DIVIDERS,
            DIVIDED_ENABLE,
            RAISED_ENABLE,
            INDEPENDENT,
        ] {
            ticks_agree(source);
        }
    }

    /// One `clk` gating `gated_clocks` clocks, each clocking a counter.
    fn gated_domains_source(gated_clocks: usize) -> String {
        let mut source = String::from(
            "module Top(input logic clk, input logic en, output logic [7:0] q);\n  \
             logic [6:0] count = 0;\n  logic [7:0] sum;\n  \
             always_ff @(posedge clk) count <= count + 7'd1;\n",
        );
        let mut terms = Vec::new();
        for index in 0..gated_clocks {
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
        source
    }

    /// An edge reaching 60 gated domains.
    #[test]
    fn an_edge_reaching_60_gated_domains_runs_as_the_plan_walk() {
        ticks_agree(&gated_domains_source(60));
    }

    /// An edge reaching more domains than one word of a fire mask holds.
    #[test]
    fn an_edge_reaching_more_than_64_domains_runs_as_the_plan_walk() {
        ticks_agree(&gated_domains_source(70));
    }

    /// The same on Cranelift, whose wide comparisons once ran every domain.
    #[test]
    fn an_edge_reaching_more_than_64_domains_runs_on_cranelift_as_the_plan_walk() {
        let source = gated_domains_source(70);
        let build =
            || Simulator::from_sv_sources(vec![(source.as_str(), Path::new("top.sv"))], "Top");
        ticks_agree_on(
            build().build_cranelift().unwrap(),
            build()
                .disable_pass(crate::SirPass::DerivedEdgeUnits)
                .build_cranelift()
                .unwrap(),
        );
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
