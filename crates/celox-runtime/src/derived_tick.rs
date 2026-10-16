//! Derived clock domains of an event-based tick.
//!
//! A tick is one active edge of an event. Clocks derived from the event run
//! with the rules of the timed scheduler:
//!
//! - A clock computed combinationally from the event's signal, such as
//!   `clk & enable`, changes at the same instant as the event. Its edge is
//!   judged from the state before the edge with the event's signal at its new
//!   level, and its domain evaluates together with the event's own domain
//!   against that state, committing with it.
//! - A clock computed from state, such as a register bit or a divider, sees
//!   its edge only after the domains that write that state commit. After each
//!   commit and combinational settle, the domains whose clocks rose (compared
//!   with the levels before the tick) run together, in rounds, until no
//!   further domain sees an edge. Each domain fires at most once per phase.
//!
//! A tick does not leave the event's signal changed. A clock that reacts to
//! the event's signal returning to its inactive level, such as `~clk`, runs
//! after the active edge's rounds, as the other half of the clock cycle the
//! tick stands for.
//!
//! What an event can reach is computed when the simulator is built
//! ([`celox_design::EventTopology::derived_events`] and
//! [`celox_design::EventTopology::combinational_derived`]), so an event with
//! no derived domains keeps the plain tick path. Gates are evaluated from a
//! small program over state variables where possible, so judging a gated
//! clock does not settle the design.

use celox_design::{DomainKind, EventTopology, GateOp};

use crate::{
    AbsoluteAddr, SignalRef, SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    simulation::{SimulationExecutor, event_level, stable_edge},
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
        Some(Self {
            eval_apply: backend.resolve_event_opt(address)?,
            eval_only: backend.resolve_eval_only_event(address),
            apply: backend.resolve_apply_event(address),
        })
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
    /// The gated domains read and write no state the ticked domain writes,
    /// nor one another's, so they may run before it, one at a time, with the
    /// result of evaluating them together.
    independent: bool,
}

/// Reusable per-tick buffers, so that a derived tick does not allocate.
#[derive(Clone, Debug, Default)]
pub struct DerivedTickScratch {
    was: Vec<bool>,
    fired: Vec<bool>,
    ready: Vec<usize>,
    gate_levels: Vec<bool>,
    gate_ready: Vec<usize>,
    /// The levers' values before a settled tick, unknown bits included.
    lever_values: Vec<(num_bigint::BigUint, num_bigint::BigUint)>,
}

impl DerivedTickScratch {
    fn prepare(&mut self, domains: usize, gated: usize, levers: usize) {
        self.was.resize(domains, false);
        self.fired.resize(domains, false);
        self.ready.clear();
        self.ready.reserve(domains);
        if gated != 0 {
            self.gate_levels.resize(gated, false);
            self.gate_ready.clear();
            self.gate_ready.reserve(gated);
        }
        if levers > self.lever_values.len() {
            self.lever_values.resize(levers, Default::default());
        }
    }
}

/// Per event ID, the derived domains a tick of that event can trigger.
/// Empty when the design has no derived clocks.
pub type DerivedTickPlans<E> = std::sync::Arc<[Option<DerivedTickPlan<E>>]>;

fn active_level(kind: Option<DomainKind>) -> u8 {
    match kind {
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => 0,
        _ => 1,
    }
}

/// Resolve the derived domains of every event of `backend`.
pub fn build_derived_tick_plans<B: SimBackend>(
    backend: &B,
    events: &EventTopology<AbsoluteAddr>,
    kinds: &fxhash::FxHashMap<AbsoluteAddr, DomainKind>,
) -> (DerivedTickPlans<B::Event>, DerivedTickScratch) {
    let mut scratch = DerivedTickScratch::default();
    if events.derived_events.is_empty() && events.combinational_derived.is_empty() {
        return (std::sync::Arc::from([]), scratch);
    }
    let two_state = !backend.layout().four_state;
    let derived = |target: &AbsoluteAddr| {
        Some(DerivedDomain {
            signal: backend.resolve_signal(target),
            kind: kinds.get(target).copied(),
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
                active_level(kinds.get(&canonical).copied()),
            )])
        };
        let fast = two_state && gated.iter().all(|gated| gated.gate.is_some());
        let independent = events.independent_derived.contains(&canonical);
        let source = EventDomain {
            eval_apply: *event,
            eval_only: backend.resolve_eval_only_event(&canonical),
            apply: backend.resolve_apply_event(&canonical),
        };
        scratch.prepare(domains.len(), gated.len(), levers.len());
        any = true;
        plans.push(Some(DerivedTickPlan {
            sources: Box::new([source]),
            gated,
            domains,
            levers,
            fast,
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
        for plan in plans {
            fast &= plan.fast;
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
            independent: false,
        }
    }

    /// Tick the source domains and every derived domain that sees its
    /// active edge. `settle` says whether combinational logic must be settled
    /// before the source domains evaluate.
    pub fn tick<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
    ) -> Result<(), SimulatorErrorCode>
    where
        X: SimulationExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        scratch.prepare(self.domains.len(), self.gated.len(), self.levers.len());
        // The baseline is the state settled after the previous operation,
        // as the timed scheduler keeps it between steps.
        self.sample(executor.backend(), scratch);
        if self.gated.is_empty() {
            fire(executor, settle, self.sources.iter())?;
            executor.eval_comb()?;
            return self.rounds(executor, scratch);
        }
        if self.fast {
            self.tick_gated_fast(executor, scratch, settle)
        } else {
            self.tick_gated_settled(executor, scratch, settle)
        }
    }

    /// Gates judged by their programs: no settle beyond the ticked domains'.
    fn tick_gated_fast<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
    ) -> Result<(), SimulatorErrorCode>
    where
        X: SimulationExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let gate_level = |backend: &X::Backend, domain: &GatedDomain<E>, active: bool| {
            let level = self.levers[domain.lever].1 ^ u8::from(!active);
            domain
                .gate
                .as_ref()
                .is_some_and(|gate| gate.level(backend, level))
        };
        // The active edge: gated clocks judged from the state before it.
        scratch.gate_ready.clear();
        for (index, domain) in self.gated.iter().enumerate() {
            let backend = executor.backend();
            let before = gate_level(backend, domain, false);
            let after = gate_level(backend, domain, true);
            if stable_edge(domain.derived.kind, before, after) {
                scratch.gate_ready.push(index);
            }
        }
        self.fire_with_gated(executor, settle, &scratch.gate_ready)?;
        executor.eval_comb()?;
        self.rounds(executor, scratch)?;

        // The return to the inactive level.
        scratch.gate_ready.clear();
        for (index, domain) in self.gated.iter().enumerate() {
            let backend = executor.backend();
            let before = gate_level(backend, domain, true);
            let after = gate_level(backend, domain, false);
            if stable_edge(domain.derived.kind, before, after) {
                scratch.gate_ready.push(index);
            }
        }
        self.fire_returned(executor, scratch)
    }

    /// Gates without programs: settle the design with the levers at each
    /// level, as the timed scheduler does at the edge.
    fn tick_gated_settled<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
        settle: bool,
    ) -> Result<(), SimulatorErrorCode>
    where
        X: SimulationExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let mut moved = false;
        for (index, &(signal, active)) in self.levers.iter().enumerate() {
            scratch.lever_values[index] = executor.backend().get_four_state(signal);
            if event_level(executor.backend(), signal) != active ^ 1 {
                executor.backend_mut().set(signal, active ^ 1);
                moved = true;
            }
        }
        if moved || settle {
            executor.eval_comb()?;
        }
        self.sample_gates(executor.backend(), scratch);
        for &(signal, active) in &self.levers {
            executor.backend_mut().set(signal, active);
        }
        executor.eval_comb()?;
        self.gates_with_edges(executor.backend(), scratch);
        self.fire_with_gated(executor, false, &scratch.gate_ready)?;
        executor.eval_comb()?;
        self.rounds(executor, scratch)?;

        self.sample_gates(executor.backend(), scratch);
        for &(signal, active) in &self.levers {
            executor.backend_mut().set(signal, active ^ 1);
        }
        executor.eval_comb()?;
        self.gates_with_edges(executor.backend(), scratch);
        self.fire_returned(executor, scratch)?;

        // Leave the levers as they were, unknown bits included.
        let mut restored = false;
        for (index, &(signal, _)) in self.levers.iter().enumerate() {
            let (value, mask) = &scratch.lever_values[index];
            if executor.backend().get_four_state(signal) != (value.clone(), mask.clone()) {
                executor
                    .backend_mut()
                    .set_four_state(signal, value.clone(), mask.clone());
                restored = true;
            }
        }
        if restored {
            executor.eval_comb()?;
        }
        Ok(())
    }

    /// Run the ticked domains together with the gated domains `ready`.
    fn fire_with_gated<X>(
        &self,
        executor: &mut X,
        settle: bool,
        ready: &[usize],
    ) -> Result<(), SimulatorErrorCode>
    where
        X: SimulationExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        let gated = ready.iter().map(|&index| &self.gated[index].derived.domain);
        if !self.independent || ready.is_empty() {
            return fire(executor, settle, self.sources.iter().chain(gated));
        }
        // Each domain finds the state the others leave alone, so running
        // them in turn with their fused kernels gives the same result as
        // evaluating them together.
        if settle {
            executor.eval_comb()?;
        }
        for domain in gated.chain(self.sources.iter()) {
            executor.eval_apply_ff_at(domain.eval_apply)?;
        }
        Ok(())
    }

    fn sample_gates<B: SimBackend>(&self, backend: &B, scratch: &mut DerivedTickScratch) {
        for (index, domain) in self.gated.iter().enumerate() {
            scratch.gate_levels[index] = event_level(backend, domain.derived.signal) != 0;
        }
    }

    fn gates_with_edges<B: SimBackend>(&self, backend: &B, scratch: &mut DerivedTickScratch) {
        scratch.gate_ready.clear();
        for (index, domain) in self.gated.iter().enumerate() {
            let is = event_level(backend, domain.derived.signal) != 0;
            if stable_edge(domain.derived.kind, scratch.gate_levels[index], is) {
                scratch.gate_ready.push(index);
            }
        }
    }

    /// Run the gated domains in `scratch.gate_ready` for the return of their
    /// sources, then their consequences.
    fn fire_returned<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
    ) -> Result<(), SimulatorErrorCode>
    where
        X: SimulationExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        if scratch.gate_ready.is_empty() {
            return Ok(());
        }
        self.sample(executor.backend(), scratch);
        fire(
            executor,
            false,
            scratch
                .gate_ready
                .iter()
                .map(|&index| &self.gated[index].derived.domain),
        )?;
        executor.eval_comb()?;
        self.rounds(executor, scratch)
    }

    fn sample<B: SimBackend>(&self, backend: &B, scratch: &mut DerivedTickScratch) {
        for (index, domain) in self.domains.iter().enumerate() {
            scratch.was[index] = event_level(backend, domain.signal) != 0;
            scratch.fired[index] = false;
        }
    }

    fn rounds<X>(
        &self,
        executor: &mut X,
        scratch: &mut DerivedTickScratch,
    ) -> Result<(), SimulatorErrorCode>
    where
        X: SimulationExecutor,
        X::Backend: SimBackend<Event = E>,
    {
        loop {
            scratch.ready.clear();
            for (index, domain) in self.domains.iter().enumerate() {
                if scratch.fired[index] {
                    continue;
                }
                let is = event_level(executor.backend(), domain.signal) != 0;
                if stable_edge(domain.kind, scratch.was[index], is) {
                    scratch.ready.push(index);
                }
            }
            if scratch.ready.is_empty() {
                return Ok(());
            }
            fire(
                executor,
                false,
                scratch
                    .ready
                    .iter()
                    .map(|&index| &self.domains[index].domain),
            )?;
            for &index in &scratch.ready {
                scratch.fired[index] = true;
            }
            executor.eval_comb()?;
        }
    }
}

/// Evaluate and commit `domains` as one coincident edge: each of them
/// samples the same state, then all of them commit.
fn fire<'a, X, E>(
    executor: &mut X,
    settle: bool,
    domains: impl Iterator<Item = &'a EventDomain<E>> + Clone,
) -> Result<(), SimulatorErrorCode>
where
    X: SimulationExecutor,
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

/// [`SimulationExecutor`] over a bare backend, for hosts without a facade.
pub struct BackendExecutor<'a, B>(pub &'a mut B);

impl<B: SimBackend> SimulationExecutor for BackendExecutor<'_, B> {
    type Backend = B;

    fn backend(&self) -> &B {
        self.0
    }

    fn backend_mut(&mut self) -> &mut B {
        self.0
    }

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.0.eval_comb()
    }

    fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), SimulatorErrorCode> {
        self.0.eval_apply_ff_at(event)
    }

    fn eval_comb_apply_ff_at(&mut self, event: B::Event) -> Result<(), SimulatorErrorCode> {
        self.0.eval_comb_apply_ff_at(event)
    }

    fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), SimulatorErrorCode> {
        self.0.eval_only_ff_at(event)
    }

    fn apply_ff_at(&mut self, event: B::Event) -> Result<(), SimulatorErrorCode> {
        self.0.apply_ff_at(event)
    }
}
