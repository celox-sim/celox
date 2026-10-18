//! Derived clock domains of an event-based tick.
//!
//! A tick is one active edge of an event. Clocks derived from the event run
//! with the rules of the timed scheduler:
//!
//! - A clock computed combinationally from the event's signal, such as
//!   `clk & enable`, changes at the same instant as the event. Its edge is
//!   judged from the state before the edge with the event's signal at its new
//!   level, and its domain evaluates together with the event's own domain
//!   against that state, committing with it (IEEE 1800-2023 4.4.2: the
//!   active region, before the nonblocking updates).
//! - After each commit and combinational settle, the domains whose clocks
//!   moved to their active edge run together, in rounds, until no further
//!   domain sees an edge: clocks computed from state, such as a register bit
//!   or a divider, compared with their levels before the tick, and gated
//!   clocks compared with their levels at the edge, so that an enable the
//!   commit raises while the clock is high is a further edge. Each domain
//!   fires at most once per phase.
//!
//! A tick is the event's active edge only and does not change the event's
//! signal. Domains that react to the signal's opposite edge, such as one
//! clocked by `~clk` or by a falling `clk & en`, do not run on a tick.
//!
//! What an event can reach is computed when the simulator is built
//! ([`celox_design::EventTopology::derived_events`] and
//! [`celox_design::EventTopology::combinational_derived`]), so an event with
//! no derived domains keeps the plain tick path. Gates are evaluated from a
//! small program over state variables where possible, so judging a gated
//! clock does not settle the design.

use celox_design::{
    DomainKind, GateOp,
    edges::{LEVEL_UNKNOWN, active_level, settled_edge},
    schedule::ScheduleModel,
};

use crate::{
    AbsoluteAddr, SignalRef, SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    simulation::{HostBoundary, UnitExecutor, edge_level},
    timing::Halt,
};

/// The kernels of one event domain.
#[derive(Clone, Copy, Debug)]
pub struct EventDomain<E> {
    pub eval_apply: E,
    pub eval_only: Option<E>,
    pub apply: Option<E>,
}

impl<E: Copy> EventDomain<E> {
    /// Resolve the kernels of the canonical domain `address`.
    pub fn resolve<B: SimBackend<Event = E>>(backend: &B, address: &AbsoluteAddr) -> Option<Self> {
        let eval_apply = backend.resolve_event_opt(address)?;
        let (eval_only, apply) = Self::split_kernels(backend, address);
        Some(Self {
            eval_apply,
            eval_only,
            apply,
        })
    }

    /// The split kernels of the canonical domain `address`: evaluation
    /// without commit, and commit.
    pub fn split_kernels<B: SimBackend<Event = E>>(
        backend: &B,
        address: &AbsoluteAddr,
    ) -> (Option<E>, Option<E>) {
        (
            backend.resolve_eval_only_event(address),
            backend.resolve_apply_event(address),
        )
    }
}

/// A derived event domain a tick may trigger.
#[derive(Clone, Copy, Debug)]
pub struct DerivedDomain<E> {
    /// The derived clock or reset whose level is sensed.
    pub signal: SignalRef,
    pub kind: Option<DomainKind>,
    pub domain: EventDomain<E>,
}

/// One step of a resolved [`GateOp`] program.
#[derive(Clone, Copy, Debug)]
enum GateInstr {
    Source,
    Load {
        signal: SignalRef,
        lsb: u32,
        mask: u64,
    },
    Const(u64),
    Not(u64),
    LogicNot,
    ReduceAnd(u64),
    ReduceOr,
    ReduceXor,
    And,
    Or,
    Xor,
    Eq,
    Ne,
    LogicAnd,
    LogicOr,
    Slice {
        lsb: u32,
        mask: u64,
    },
    Mux,
}

const GATE_STACK: usize = 16;

fn mask(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// A derived clock's value as a function of its source's level, evaluated
/// from state variables without settling the design.
#[derive(Clone, Debug)]
pub struct Gate {
    program: Box<[GateInstr]>,
}

impl Gate {
    fn resolve<B: SimBackend>(backend: &B, ops: &[GateOp<AbsoluteAddr>]) -> Option<Self> {
        let mut depth = 0usize;
        let mut program = Vec::with_capacity(ops.len());
        for op in ops {
            let (instr, pops) = match *op {
                GateOp::Source => (GateInstr::Source, 0),
                GateOp::Load {
                    address,
                    lsb,
                    width,
                } => {
                    let signal = backend.resolve_signal(&address);
                    if signal.width > 64
                        || signal.array_layout.is_some()
                        || (lsb + width) as usize > signal.width
                    {
                        return None;
                    }
                    (
                        GateInstr::Load {
                            signal,
                            lsb,
                            mask: mask(width),
                        },
                        0,
                    )
                }
                GateOp::Const(value) => (GateInstr::Const(value), 0),
                GateOp::Not { width } => (GateInstr::Not(mask(width)), 1),
                GateOp::LogicNot => (GateInstr::LogicNot, 1),
                GateOp::ReduceAnd { width } => (GateInstr::ReduceAnd(mask(width)), 1),
                GateOp::ReduceOr => (GateInstr::ReduceOr, 1),
                GateOp::ReduceXor => (GateInstr::ReduceXor, 1),
                GateOp::And => (GateInstr::And, 2),
                GateOp::Or => (GateInstr::Or, 2),
                GateOp::Xor => (GateInstr::Xor, 2),
                GateOp::Eq => (GateInstr::Eq, 2),
                GateOp::Ne => (GateInstr::Ne, 2),
                GateOp::LogicAnd => (GateInstr::LogicAnd, 2),
                GateOp::LogicOr => (GateInstr::LogicOr, 2),
                GateOp::Slice { lsb, width } => (
                    GateInstr::Slice {
                        lsb,
                        mask: mask(width),
                    },
                    1,
                ),
                GateOp::Mux => (GateInstr::Mux, 3),
            };
            depth = depth.checked_sub(pops)? + 1;
            if depth > GATE_STACK {
                return None;
            }
            program.push(instr);
        }
        (depth == 1).then(|| Self {
            program: program.into(),
        })
    }

    /// The clock's level with its source at `source` (0 or 1).
    fn level<B: SimBackend>(&self, backend: &B, source: u8) -> bool {
        let mut stack = [0u64; GATE_STACK];
        let mut top = 0usize;
        for instr in &self.program {
            let value = match *instr {
                GateInstr::Source => u64::from(source),
                GateInstr::Load { signal, lsb, mask } => {
                    (backend.get_as::<u64>(signal) >> lsb) & mask
                }
                GateInstr::Const(value) => value,
                GateInstr::Not(mask) => {
                    top -= 1;
                    !stack[top] & mask
                }
                GateInstr::LogicNot => {
                    top -= 1;
                    u64::from(stack[top] == 0)
                }
                GateInstr::ReduceAnd(mask) => {
                    top -= 1;
                    u64::from(stack[top] & mask == mask)
                }
                GateInstr::ReduceOr => {
                    top -= 1;
                    u64::from(stack[top] != 0)
                }
                GateInstr::ReduceXor => {
                    top -= 1;
                    u64::from(stack[top].count_ones() & 1)
                }
                GateInstr::Slice { lsb, mask } => {
                    top -= 1;
                    (stack[top] >> lsb) & mask
                }
                GateInstr::Mux => {
                    top -= 3;
                    if stack[top] != 0 {
                        stack[top + 1]
                    } else {
                        stack[top + 2]
                    }
                }
                binary => {
                    top -= 2;
                    let (lhs, rhs) = (stack[top], stack[top + 1]);
                    match binary {
                        GateInstr::And => lhs & rhs,
                        GateInstr::Or => lhs | rhs,
                        GateInstr::Xor => lhs ^ rhs,
                        GateInstr::Eq => u64::from(lhs == rhs),
                        GateInstr::Ne => u64::from(lhs != rhs),
                        GateInstr::LogicAnd => u64::from(lhs != 0 && rhs != 0),
                        GateInstr::LogicOr => u64::from(lhs != 0 || rhs != 0),
                        _ => unreachable!("every other instruction is handled above"),
                    }
                }
            };
            stack[top] = value;
            top += 1;
        }
        stack[0] & 1 != 0
    }
}

/// A clock computed combinationally from a ticked event's signal.
#[derive(Clone, Debug)]
pub struct GatedDomain<E> {
    pub derived: DerivedDomain<E>,
    /// Index of the source's signal in [`DerivedTickPlan::levers`].
    lever: usize,
    gate: Option<Gate>,
}

/// What a tick of an event, or of coincident events, triggers besides the
/// ticked domains themselves.
#[derive(Clone, Debug)]
pub struct DerivedTickPlan<E> {
    /// The ticked domains.
    sources: Box<[EventDomain<E>]>,
    /// Clocks computed combinationally from a ticked signal.
    gated: Box<[GatedDomain<E>]>,
    /// Domains clocked from state the ticked domains write.
    domains: Box<[DerivedDomain<E>]>,
    /// Ticked signals that gate [`Self::gated`], with their active level.
    levers: Box<[(SignalRef, u8)]>,
    /// Every gate has a program over two-state values, so gates are judged
    /// without settling the design.
    fast: bool,
    /// The design keeps unknown bits: levels are read with their masks and
    /// edges follow the four-state rows of IEEE 1800-2023 Table 9-2.
    four_state: bool,
    /// The gated domains read and write no state the ticked domain writes,
    /// nor one another's, so they may run before it, one at a time, with the
    /// result of evaluating them together.
    independent: bool,
}

/// Reusable per-tick buffers, so that a derived tick does not allocate. It
/// is kept small: hosts move it in and out of themselves on every tick.
#[derive(Clone, Debug, Default)]
pub struct DerivedTickScratch {
    /// Per round entry ([`DerivedTickPlan::domains`], then
    /// [`DerivedTickPlan::gated`]): [`WAS`] and [`FIRED`]. After them, the
    /// value and unknown mask of each lever saved by a settled tick.
    flags: Vec<u8>,
    ready: Vec<usize>,
}

/// The entry's edge level (see [`edge_level`]) at the start of the
/// current phase.
const WAS: u8 = 1 | LEVEL_UNKNOWN;
/// The entry's domain already ran in the current phase.
const FIRED: u8 = 4;

impl DerivedTickScratch {
    #[inline]
    fn prepare(&mut self, entries: usize, levers: usize) {
        self.flags.resize(entries + 2 * levers, 0);
        self.ready.clear();
        self.ready.reserve(entries);
    }
}

/// Per event ID, the derived domains a tick of that event can trigger.
/// Empty when the design has no derived clocks.
pub type DerivedTickPlans<E> = std::sync::Arc<[Option<DerivedTickPlan<E>>]>;

/// Resolve the derived domains of every event of `backend`.
pub fn build_derived_tick_plans<B: SimBackend>(
    backend: &B,
    model: &ScheduleModel<AbsoluteAddr>,
) -> (DerivedTickPlans<B::Event>, DerivedTickScratch) {
    let events = &model.events;
    let mut scratch = DerivedTickScratch::default();
    if events.derived_events.is_empty() && events.combinational_derived.is_empty() {
        return (std::sync::Arc::from([]), scratch);
    }
    let two_state = !backend.layout().four_state;
    let derived = |target: &AbsoluteAddr| {
        Some(DerivedDomain {
            signal: backend.resolve_signal(target),
            kind: model.kind(target),
            domain: EventDomain::resolve(backend, target)?,
        })
    };
    let mut plans = Vec::with_capacity(backend.num_events());
    let mut any = false;
    for (address, event) in backend
        .id_to_addr_slice()
        .iter()
        .zip(backend.id_to_event_slice())
    {
        let canonical = events.canonical(*address);
        let domains: Box<[_]> = events
            .derived_events
            .get(&canonical)
            .into_iter()
            .flatten()
            .filter_map(derived)
            .collect();
        let gated: Box<[_]> = events
            .combinational_derived
            .get(&canonical)
            .into_iter()
            .flatten()
            .filter_map(|clock| {
                Some(GatedDomain {
                    derived: derived(&clock.clock)?,
                    lever: 0,
                    gate: clock
                        .gate
                        .as_deref()
                        .and_then(|ops| Gate::resolve(backend, ops)),
                })
            })
            .collect();
        if domains.is_empty() && gated.is_empty() {
            plans.push(None);
            continue;
        }
        let levers: Box<[_]> = if gated.is_empty() {
            Box::new([])
        } else {
            Box::new([(
                backend.resolve_signal(&canonical),
                active_level(model.kind(&canonical)),
            )])
        };
        let fast = two_state && gated.iter().all(|gated| gated.gate.is_some());
        let independent = events.independent_derived.contains(&canonical);
        let (eval_only, apply) = EventDomain::split_kernels(backend, &canonical);
        let source = EventDomain {
            eval_apply: *event,
            eval_only,
            apply,
        };
        scratch.prepare(domains.len() + gated.len(), levers.len());
        any = true;
        plans.push(Some(DerivedTickPlan {
            sources: Box::new([source]),
            gated,
            domains,
            levers,
            fast,
            four_state: !two_state,
            independent,
        }));
    }
    if !any {
        return (std::sync::Arc::from([]), scratch);
    }
    (plans.into(), scratch)
}

impl<E: EventHandle> DerivedTickPlan<E> {
    /// The plan of coincident `sources`, merging their own plans. Derived
    /// domains are listed once, and the ticked domains themselves are left
    /// out.
    pub fn merge<'a>(
        sources: Vec<EventDomain<E>>,
        plans: impl IntoIterator<Item = &'a Self>,
    ) -> Self
    where
        E: 'a,
    {
        let ticked = |domain: &EventDomain<E>| {
            let id = domain.eval_apply.id();
            sources.iter().any(|source| source.eval_apply.id() == id)
        };
        let mut gated: Vec<GatedDomain<E>> = Vec::new();
        let mut domains: Vec<DerivedDomain<E>> = Vec::new();
        let mut levers: Vec<(SignalRef, u8)> = Vec::new();
        let mut fast = true;
        let mut four_state = false;
        for plan in plans {
            fast &= plan.fast;
            four_state |= plan.four_state;
            for domain in &plan.gated {
                let id = domain.derived.domain.eval_apply.id();
                if ticked(&domain.derived.domain)
                    || gated
                        .iter()
                        .any(|known| known.derived.domain.eval_apply.id() == id)
                {
                    continue;
                }
                let lever = plan.levers[domain.lever];
                let index = levers.iter().position(|known| *known == lever);
                let index = index.unwrap_or_else(|| {
                    levers.push(lever);
                    levers.len() - 1
                });
                gated.push(GatedDomain {
                    lever: index,
                    ..domain.clone()
                });
            }
            for domain in &plan.domains {
                let id = domain.domain.eval_apply.id();
                if !ticked(&domain.domain)
                    && domains
                        .iter()
                        .all(|known| known.domain.eval_apply.id() != id)
                {
                    domains.push(*domain);
                }
            }
        }
        // A gate program reads every other ticked signal at its stored
        // level, so several gating signals settle the design instead.
        fast &= levers.len() <= 1;
        Self {
            sources: sources.into(),
            gated: gated.into(),
            domains: domains.into(),
            levers: levers.into(),
            fast,
            four_state,
            independent: false,
        }
    }

    /// Tick the source domains and every derived domain that sees its
    /// active edge, without moving the ticked signals. `settle` says whether
    /// combinational logic must be settled before the source domains
    /// evaluate.
    pub fn tick<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
    ) -> Result<(), SimulatorErrorCode>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let pulse = |&(_, active): &(SignalRef, u8)| (active ^ 1, active);
        fn run(transitions: &[(u8, u8)]) -> EdgeRun<'_> {
            EdgeRun {
                transitions,
                persist: false,
                fire_sources: true,
            }
        }
        let result = match self.levers.as_ref() {
            [] => self.run(executor, scratch, settle, run(&[])),
            [lever] => self.run(executor, scratch, settle, run(&[pulse(lever)])),
            levers => {
                let transitions: Vec<_> = levers.iter().map(pulse).collect();
                self.run(executor, scratch, settle, run(&transitions))
            }
        };
        match result {
            // An RTL `$finish` or `$stop` ends or suspends only a timed
            // simulation, which does not tick; a tick leaves it to the host.
            Ok(_) | Err(Halt::Finish | Halt::Stop) => Ok(()),
            Err(Halt::Error(error)) => Err(error),
        }
    }

    /// Ticked signals that gate derived clocks, in the order
    /// [`EdgeRun::transitions`] lists their levels.
    pub fn lever_signals(&self) -> impl Iterator<Item = SignalRef> + '_ {
        self.levers.iter().map(|&(signal, _)| signal)
    }

    /// Run one edge of the ticked signals: the source domains when
    /// `run.fire_sources`, every gated clock that sees its active edge in
    /// `run.transitions`, then the cascade rounds. Returns whether
    /// combinational logic is settled afterwards; it is not when nothing ran.
    ///
    /// The levels of [`EdgeRun::transitions`] are edge levels (see
    /// [`celox_design::edges`]): a lever may leave x or z.
    pub fn run<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
        run: EdgeRun<'_>,
    ) -> Result<bool, Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        if self.four_state {
            self.run_in::<true, X>(executor, scratch, settle, run)
        } else {
            self.run_in::<false, X>(executor, scratch, settle, run)
        }
    }

    /// [`Self::run`] with levels read in the design's state mode.
    fn run_in<const FOUR_STATE: bool, X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
        run: EdgeRun<'_>,
    ) -> Result<bool, Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        scratch.prepare(self.domains.len() + self.gated.len(), self.levers.len());
        // The baseline is the state settled after the previous operation,
        // as the timed scheduler keeps it between steps.
        self.sample_domains::<FOUR_STATE, _>(executor.backend(), scratch);
        let sources: &[EventDomain<E>] = if run.fire_sources { &self.sources } else { &[] };
        if self.gated.is_empty() {
            if sources.is_empty() {
                return Ok(false);
            }
            fire(executor, settle, sources.iter())?;
            executor.eval_comb()?;
            self.rounds::<FOUR_STATE, X>(executor, scratch, run.transitions)?;
            return Ok(true);
        }
        if self.fast {
            self.run_gated_fast::<FOUR_STATE, X>(
                executor,
                scratch,
                settle,
                sources,
                run.transitions,
            )?;
        } else {
            self.run_gated_settled::<FOUR_STATE, X>(executor, scratch, settle, sources, &run)?;
        }
        Ok(true)
    }

    /// Gates judged by their programs: no settle beyond the ticked domains'.
    fn run_gated_fast<const FOUR_STATE: bool, X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
        sources: &[EventDomain<E>],
        transitions: &[(u8, u8)],
    ) -> Result<(), Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        // The edge: gated clocks judged from the state before it.
        let base = self.domains.len();
        scratch.ready.clear();
        for (index, domain) in self.gated.iter().enumerate() {
            let (from, to) = transitions[domain.lever];
            let backend = executor.backend();
            let before = self.gate_level::<FOUR_STATE, _>(backend, domain, from);
            let after = self.gate_level::<FOUR_STATE, _>(backend, domain, to);
            let fired = settled_edge::<FOUR_STATE>(domain.derived.kind, before, after);
            if fired {
                scratch.ready.push(index);
            }
            scratch.flags[base + index] = flags(after, fired);
        }
        self.fire_with_gated(executor, settle, sources, &scratch.ready)?;
        executor.eval_comb()?;
        // The commit may move a gate, as the nonblocking updates after the
        // edge do; that is a further edge of the gated clock.
        self.rounds::<FOUR_STATE, X>(executor, scratch, transitions)
    }

    /// Gates without programs: settle the design with the levers at each
    /// level, as the timed scheduler does at the edge.
    fn run_gated_settled<const FOUR_STATE: bool, X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
        sources: &[EventDomain<E>],
        run: &EdgeRun<'_>,
    ) -> Result<(), Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let base = self.domains.len();
        let saved = base + self.gated.len();
        let mut moved = false;
        for (index, &(signal, _)) in self.levers.iter().enumerate() {
            let (from, _) = run.transitions[index];
            if !run.persist {
                let (value, mask) = executor.backend().get_four_state(signal);
                scratch.flags[saved + 2 * index] = u8::try_from(&value).unwrap_or(0);
                scratch.flags[saved + 2 * index + 1] = u8::try_from(&mask).unwrap_or(0);
            }
            if edge_level::<FOUR_STATE, _>(executor.backend(), signal) != from {
                if FOUR_STATE && from & LEVEL_UNKNOWN != 0 {
                    // The lever leaves x or z.
                    executor.backend_mut().set_four_state(
                        signal,
                        num_bigint::BigUint::from(from & 1),
                        num_bigint::BigUint::from(1u8),
                    );
                } else {
                    executor.backend_mut().set(signal, from);
                }
                moved = true;
            }
        }
        if moved || settle {
            executor.eval_comb()?;
        }
        for (index, domain) in self.gated.iter().enumerate() {
            let level = edge_level::<FOUR_STATE, _>(executor.backend(), domain.derived.signal);
            scratch.flags[base + index] = flags(level, false);
        }
        for (index, &(signal, _)) in self.levers.iter().enumerate() {
            executor.backend_mut().set(signal, run.transitions[index].1);
        }
        executor.eval_comb()?;
        scratch.ready.clear();
        for (index, domain) in self.gated.iter().enumerate() {
            let before = scratch.flags[base + index] & WAS;
            let after = edge_level::<FOUR_STATE, _>(executor.backend(), domain.derived.signal);
            let fired = settled_edge::<FOUR_STATE>(domain.derived.kind, before, after);
            if fired {
                scratch.ready.push(index);
            }
            scratch.flags[base + index] = flags(after, fired);
        }
        self.fire_with_gated(executor, false, sources, &scratch.ready)?;
        executor.eval_comb()?;
        self.rounds::<FOUR_STATE, X>(executor, scratch, run.transitions)?;
        if run.persist {
            return Ok(());
        }

        // Leave the levers as they were, unknown bits included.
        let mut restored = false;
        for (index, &(signal, _)) in self.levers.iter().enumerate() {
            let value = num_bigint::BigUint::from(scratch.flags[saved + 2 * index]);
            let mask = num_bigint::BigUint::from(scratch.flags[saved + 2 * index + 1]);
            if executor.backend().get_four_state(signal) != (value.clone(), mask.clone()) {
                executor.backend_mut().set_four_state(signal, value, mask);
                restored = true;
            }
        }
        if restored {
            executor.eval_comb()?;
        }
        Ok(())
    }

    /// Run `sources` together with the gated domains `ready`.
    fn fire_with_gated<X>(
        &self,
        executor: &mut X,
        settle: bool,
        sources: &[EventDomain<E>],
        ready: &[usize],
    ) -> Result<(), Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let gated = ready.iter().map(|&index| &self.gated[index].derived.domain);
        if !self.independent || ready.is_empty() {
            return fire(executor, settle, sources.iter().chain(gated));
        }
        // Each domain finds the state the others leave alone, so running
        // them in turn with their fused kernels gives the same result as
        // evaluating them together.
        if settle {
            executor.eval_comb()?;
        }
        for domain in gated.chain(sources.iter()) {
            executor.eval_apply_ff_at(domain.eval_apply)?;
        }
        Ok(())
    }

    /// A gated clock's level with its lever at `level`, from its program in
    /// the fast path and from the settled design otherwise.
    #[inline]
    fn gate_level<const FOUR_STATE: bool, B: SimBackend>(
        &self,
        backend: &B,
        domain: &GatedDomain<E>,
        level: u8,
    ) -> u8 {
        match &domain.gate {
            Some(gate) if self.fast => u8::from(gate.level(backend, level)),
            _ => edge_level::<FOUR_STATE, _>(backend, domain.derived.signal),
        }
    }

    #[inline]
    fn sample_domains<const FOUR_STATE: bool, B: SimBackend>(
        &self,
        backend: &B,
        scratch: &mut DerivedTickScratch,
    ) {
        for (index, domain) in self.domains.iter().enumerate() {
            let level = edge_level::<FOUR_STATE, _>(backend, domain.signal);
            scratch.flags[index] = flags(level, false);
        }
    }

    /// Cascade rounds over the state-derived and gated domains: each round
    /// runs together every domain whose clock moved to its active edge since
    /// the phase began, then settles, until no further edge appears.
    fn rounds<const FOUR_STATE: bool, X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        transitions: &[(u8, u8)],
    ) -> Result<(), Halt>
    where
        X: TickExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let base = self.domains.len();
        loop {
            scratch.ready.clear();
            for (index, domain) in self.domains.iter().enumerate() {
                let state = scratch.flags[index];
                if state & FIRED != 0 {
                    continue;
                }
                let is = edge_level::<FOUR_STATE, _>(executor.backend(), domain.signal);
                if settled_edge::<FOUR_STATE>(domain.kind, state & WAS, is) {
                    scratch.ready.push(index);
                }
            }
            for (index, domain) in self.gated.iter().enumerate() {
                let state = scratch.flags[base + index];
                if state & FIRED != 0 {
                    continue;
                }
                let is = self.gate_level::<FOUR_STATE, _>(
                    executor.backend(),
                    domain,
                    transitions[domain.lever].1,
                );
                if settled_edge::<FOUR_STATE>(domain.derived.kind, state & WAS, is) {
                    scratch.ready.push(base + index);
                }
            }
            if scratch.ready.is_empty() {
                return Ok(());
            }
            fire(
                executor,
                false,
                scratch.ready.iter().map(|&entry| {
                    if entry < base {
                        &self.domains[entry].domain
                    } else {
                        &self.gated[entry - base].derived.domain
                    }
                }),
            )?;
            for &entry in &scratch.ready {
                scratch.flags[entry] |= FIRED;
            }
            executor.eval_comb()?;
        }
    }
}

/// How an edge moves the signals that gate a plan's derived clocks.
#[derive(Clone, Copy, Debug)]
pub struct EdgeRun<'a> {
    /// Per lever ([`DerivedTickPlan::lever_signals`]), its edge level (see
    /// [`celox_design::edges`]) before and after the edge.
    pub transitions: &'a [(u8, u8)],
    /// The levers stay at their new level; otherwise they are left as they
    /// were before the edge.
    pub persist: bool,
    /// The ticked domains themselves see their active edge.
    pub fire_sources: bool,
}

#[inline]
fn flags(level: u8, fired: bool) -> u8 {
    level | (u8::from(fired) * FIRED)
}

/// Evaluate and commit `domains` as one coincident edge: each of them
/// samples the same state, then all of them commit.
// Instantiated for each state mode, it would otherwise stay out of line,
// which costs two-state ticks the call.
#[inline(always)]
fn fire<'a, X, E>(
    executor: &mut X,
    settle: bool,
    domains: impl Iterator<Item = &'a EventDomain<E>> + Clone,
) -> Result<(), Halt>
where
    X: TickExecutor,
    X::Backend: SimBackend<Event = E>,
    E: 'a + Copy,
{
    let mut probe = domains.clone();
    match (probe.next(), probe.next()) {
        (None, _) => {
            if settle {
                executor.eval_comb()?;
            }
            Ok(())
        }
        (Some(domain), None) => {
            if settle {
                executor.eval_comb_apply_ff_at(domain.eval_apply)
            } else {
                executor.eval_apply_ff_at(domain.eval_apply)
            }
        }
        _ => {
            if settle {
                executor.eval_comb()?;
            }
            for domain in domains.clone() {
                match domain.eval_only {
                    Some(event) => executor.eval_only_ff_at(event)?,
                    None => executor.eval_apply_ff_at(domain.eval_apply)?,
                }
            }
            for domain in domains {
                if let Some(event) = domain.apply {
                    executor.apply_ff_at(event)?;
                }
            }
            Ok(())
        }
    }
}

/// The units a derived tick executes.
pub trait TickExecutor: UnitExecutor {
    /// Settle combinational logic, then evaluate and commit one domain.
    fn eval_comb_apply_ff_at(
        &mut self,
        event: <Self::Backend as SimBackend>::Event,
    ) -> Result<(), Halt> {
        self.eval_comb()?;
        self.eval_apply_ff_at(event)
    }
}

/// [`crate::SimulationExecutor`] over a bare backend, for hosts without a
/// facade.
pub struct BackendExecutor<'a, B>(pub &'a mut B);

impl<B: SimBackend> UnitExecutor for BackendExecutor<'_, B> {
    type Backend = B;

    fn backend(&self) -> &B {
        self.0
    }

    fn backend_mut(&mut self) -> &mut B {
        self.0
    }

    fn eval_comb(&mut self) -> Result<(), Halt> {
        Ok(self.0.eval_comb()?)
    }

    fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        Ok(self.0.eval_apply_ff_at(event)?)
    }

    fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        Ok(self.0.eval_only_ff_at(event)?)
    }

    fn apply_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        Ok(self.0.apply_ff_at(event)?)
    }
}

impl<B: SimBackend> TickExecutor for BackendExecutor<'_, B> {
    fn eval_comb_apply_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        Ok(self.0.eval_comb_apply_ff_at(event)?)
    }
}

impl<B: SimBackend> HostBoundary for BackendExecutor<'_, B> {}
