//! Hooks for the tests of other modules into the plan and the generator.
use super::*;

/// Instrument the generated settle after eligibility/retention analysis, so
/// measuring it does not turn a pure combinational cone into a stateful one.
#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn count_test_settles(
    program: &mut LaidOutProgram,
    counter: AbsoluteAddr,
    fused_counter: AbsoluteAddr,
) -> usize {
    fn counted(units: &[Unit], counter: AbsoluteAddr) -> Vec<Unit> {
        let mut b = Builder::new();
        let count = b.alloc_bit(64, false);
        let one = b.alloc_bit(64, false);
        let next = b.alloc_bit(64, false);
        let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, counter);
        b.emit(SIRInstruction::Load(
            count,
            address,
            SIROffset::Static(0),
            64,
        ));
        b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        b.emit(SIRInstruction::Binary(next, count, BinaryOp::Add, one));
        b.emit(SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            64,
            next,
            vec![],
            vec![],
        ));
        for unit in units {
            b.inline_execution_unit(unit);
        }
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        vec![Unit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        }]
    }
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let mut plan = Plan::new(program, &schema.slots, true).unwrap();
    let mut fused = 0;
    if let Some(lazy) = &mut plan.lazy {
        for domain in &mut lazy.domains {
            if let DomainSettle::Fused(units) = domain {
                *units = counted(units, fused_counter);
                fused += 1;
            }
        }
    }
    let instrumented = counted(&program.sir.eval_comb, counter);
    let original = std::mem::replace(&mut program.sir.eval_comb, instrumented);
    let generated = plan.generate(program, &schema.slots);
    program.sir.eval_comb = original;
    program.sir.processes[schema.unit.unwrap()] = generated;
    fused
}

/// What [`Plan::new`] decides for `program` that tests of call effects
/// check: the storage the settle reads, why each kernel falls back, and the
/// events RTL logic drives (`Plan::derived`).
#[cfg(test)]
pub(crate) struct PlanSummary {
    pub(crate) settle_reads: Option<HashSet<AbsoluteAddr>>,
    pub(crate) fallback: Vec<Option<String>>,
    pub(crate) derived: Vec<AbsoluteAddr>,
    /// With deferred settles, per kernel whether its runs observe the
    /// settled state (`Lazy::kernels`).
    pub(crate) lazy: Option<Vec<bool>>,
    pub(crate) incremental: bool,
}

#[cfg(test)]
pub(crate) fn plan_for_test(program: &LaidOutProgram) -> Result<PlanSummary, String> {
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let plan = Plan::new(program, &schema.slots, true)?;
    Ok(PlanSummary {
        derived: plan
            .derived
            .iter()
            .map(|&event| plan.events[event].signal)
            .collect(),
        lazy: plan.lazy.as_ref().map(|lazy| lazy.kernels.clone()),
        incremental: plan.incremental.is_some(),
        settle_reads: plan.settle_reads,
        fallback: plan.fallback,
    })
}

/// Whether the compiled scheduler of `program` defers its settles, and
/// whether its loads alone would have allowed that.
#[cfg(test)]
pub(crate) fn defers_settles_for_test(program: &LaidOutProgram) -> (bool, bool) {
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let plan = Plan::new(program, &schema.slots, true).unwrap();
    let shared = program.shared_storage();
    let storage = |address: AbsoluteAddr| shared.get(&address).copied().unwrap_or(address);
    let stores: HashSet<AbsoluteAddr> = program
        .sir
        .eval_comb
        .iter()
        .flat_map(instructions)
        .flat_map(written_storage)
        .map(|write| storage(write.absolute_addr()))
        .collect();
    (
        plan.lazy.is_some(),
        !retains_state(&program.sir.eval_comb, &storage, &stores),
    )
}

#[cfg(all(
    test,
    feature = "host-runtime",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(crate) fn incremental_inputs_for_test(program: &LaidOutProgram) -> Option<usize> {
    let schema = program.runtime.runtime_schema.scheduler.as_ref().unwrap();
    Plan::new(program, &schema.slots, true)
        .unwrap()
        .incremental
        .map(|cone| cone.sources.len())
}

/// Rebuild only the scheduler without occurrence-probe hints. The design's
/// instrumented stores stay intact; metadata can be omitted to exercise the
/// conservative statefulness check.
#[cfg(all(
    test,
    feature = "host-runtime",
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]
pub(crate) fn without_probe_hints_for_test(
    mut program: LaidOutProgram,
    statefulness_known: bool,
) -> LaidOutProgram {
    program.runtime.runtime_schema.occurrence_probes.clear();
    if !statefulness_known {
        program.sir.eval_comb_stateful = None;
    }
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let plan = Plan::new(&program, &schema.slots, true).unwrap();
    assert_eq!(schema.fallback, plan.fallback);
    assert_eq!(
        schema.events,
        plan.tracked()
            .iter()
            .map(|&event| plan.events[event].signal)
            .collect::<Vec<_>>()
    );
    program.sir.processes[schema.unit.unwrap()] = plan.generate(&program, &schema.slots);
    program
}
