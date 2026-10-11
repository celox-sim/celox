//! Optimization of lane-partitioned kernels.
//!
//! Every lane unit becomes part of a separately compiled function which may
//! run concurrently with units of other lanes; their order is derived later
//! from final memory effects. Only passes whose reasoning stays inside one
//! unit are applied. Combinational units use the sequential comb pipeline and
//! the per-unit late stages; program-wide identity aliasing is decided once
//! from the sequential kernel. FF units use the split-path evaluate and apply
//! pipelines, which never assume that a unit observes its own publication.

use super::*;
use crate::optimizer::passes::control_flow::{pass_branchify_mux, pass_guarded_region_sinking};
use celox_sir::LaneUnit;

fn run_lane_units(
    units: &mut Vec<LaneUnit<RegionedAbsoluteAddr>>,
    passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    if units.is_empty() {
        return;
    }
    let lanes = units.iter().map(|unit| unit.lane).collect::<Vec<_>>();
    let mut plain = std::mem::take(units)
        .into_iter()
        .map(|unit| unit.unit)
        .collect::<Vec<_>>();
    passes.run_parallel(&mut plain, options);
    *units = lanes
        .into_iter()
        .zip(plain)
        .map(|(lane, unit)| LaneUnit::new(lane, unit))
        .collect();
}

/// Run the per-unit pipelines over every partitioned kernel.
pub(super) fn optimize_parallel_units(
    program: &mut OptimizationContext<'_>,
    comb_passes: &ExecutionUnitPassManager,
    eval_only_passes: &ExecutionUnitPassManager,
    apply_passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    let Some(parallel) = program.sir.parallel.as_mut() else {
        return;
    };
    run_lane_units(&mut parallel.eval_comb, comb_passes, options);
    for kernel in parallel.eval_apply_ffs.values_mut() {
        run_lane_units(&mut kernel.evaluations, eval_only_passes, options);
        run_lane_units(&mut kernel.applications, apply_passes, options);
    }
}

/// Let lane FF evaluations publish their next state directly.
///
/// A partitioned FF part is an evaluation, which stages its next state in
/// WORKING or sparse storage, and an application, which commits it to STABLE
/// after every unit that reads the old value. The two usually end up in
/// different tasks, so the per-function round-trip elimination of the
/// sequential kernel never sees them together, and every target pays for a
/// seed copy, a staged store and a publishing copy.
///
/// A target may instead be written to STABLE by its evaluation when the
/// evaluation followed by its application passes the sequential
/// round-trip elimination, which proves that the part itself never observes
/// the old value after writing the new one, and no later unit of the kernel
/// in sequential order (a later evaluation or another application) accesses
/// the target, and no other unit touches its staged storage. Earlier readers in other lanes stay correct because task
/// ordering is derived from the final memory effects, which now include the
/// evaluation's STABLE write.
pub(super) fn publish_parallel_ff_directly(program: &mut OptimizationContext<'_>) {
    let Some(parallel) = program.sir.parallel.as_mut() else {
        return;
    };
    for kernel in parallel.eval_apply_ffs.values_mut() {
        publish_kernel_directly(kernel);
    }
}

fn publish_kernel_directly(kernel: &mut celox_sir::ParallelFfKernel<RegionedAbsoluteAddr>) {
    use crate::optimizer::pass_eliminate_working_round_trip::eliminate_working_round_trip;

    if kernel.evaluations.len() != kernel.applications.len() {
        return;
    }
    let accessed = |unit: &ExecutionUnit<RegionedAbsoluteAddr>| {
        let mut objects = crate::HashSet::<AbsoluteAddr>::default();
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            match instruction {
                SIRInstruction::Load(_, address, ..) | SIRInstruction::Store(address, ..) => {
                    objects.insert(address.absolute_addr());
                }
                SIRInstruction::Commit(source, destination, ..) => {
                    objects.insert(source.absolute_addr());
                    objects.insert(destination.absolute_addr());
                }
                _ => {}
            }
        }
        objects
    };
    let evaluation_objects = kernel
        .evaluations
        .iter()
        .map(|unit| accessed(&unit.unit))
        .collect::<Vec<_>>();
    let application_objects = kernel
        .applications
        .iter()
        .map(|unit| accessed(&unit.unit))
        .collect::<Vec<_>>();
    // Last evaluation, and number of applications, accessing each object.
    let mut last_evaluation = crate::HashMap::<AbsoluteAddr, usize>::default();
    for (index, objects) in evaluation_objects.iter().enumerate() {
        for &object in objects {
            last_evaluation.insert(object, index);
        }
    }
    let mut application_count = crate::HashMap::<AbsoluteAddr, usize>::default();
    for objects in &application_objects {
        for &object in objects {
            *application_count.entry(object).or_default() += 1;
        }
    }
    // Units touching each object's staged (WORKING or sparse) storage.
    let mut staged_users = crate::HashMap::<AbsoluteAddr, usize>::default();
    for unit in kernel.units() {
        let mut staged = crate::HashSet::<AbsoluteAddr>::default();
        for instruction in unit
            .unit
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
        {
            let addresses = match instruction {
                SIRInstruction::Load(_, address, ..) | SIRInstruction::Store(address, ..) => {
                    [Some(address), None]
                }
                SIRInstruction::Commit(source, destination, ..) => {
                    [Some(source), Some(destination)]
                }
                _ => [None, None],
            };
            for address in addresses.into_iter().flatten() {
                if matches!(address.region, WORKING_REGION | SPARSE_WORKING_REGION) {
                    staged.insert(address.absolute_addr());
                }
            }
        }
        for object in staged {
            *staged_users.entry(object).or_default() += 1;
        }
    }

    for index in 0..kernel.evaluations.len() {
        let evaluation = &kernel.evaluations[index].unit;
        let application = &kernel.applications[index].unit;
        // Staged targets published by this application whose only later
        // access in sequential order is that publication.
        let candidates = application
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Commit(source, destination, SIROffset::Static(0), _, triggers)
                    if matches!(source.region, WORKING_REGION | SPARSE_WORKING_REGION)
                        && destination.region == STABLE_REGION
                        && source.absolute_addr() == destination.absolute_addr()
                        && triggers.is_empty() =>
                {
                    Some(source.absolute_addr())
                }
                _ => None,
            })
            .filter(|object| {
                last_evaluation.get(object) == Some(&index)
                    && application_count.get(object) == Some(&1)
                    && staged_users.get(object).is_some_and(|&users| users <= 2)
            })
            .collect::<crate::HashSet<_>>();
        if candidates.is_empty() {
            continue;
        }
        // The sequential elimination decides which candidates the part
        // itself allows to publish directly.
        let (mut merged, boundaries) = celox_sir::merge_sir_eu_refs(&[evaluation, application]);
        eliminate_working_round_trip(&mut merged, &boundaries);
        let still_published = merged
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Commit(source, destination, ..)
                    if destination.region == STABLE_REGION =>
                {
                    Some(source.absolute_addr())
                }
                _ => None,
            })
            .collect::<crate::HashSet<_>>();
        let direct = candidates
            .into_iter()
            .filter(|object| !still_published.contains(object))
            .collect::<crate::HashSet<_>>();
        if direct.is_empty() {
            continue;
        }
        rewrite_direct_publication(&mut kernel.evaluations[index].unit, &direct);
        rewrite_direct_publication(&mut kernel.applications[index].unit, &direct);
    }
}

/// Redirect staged accesses of `direct` targets to STABLE and drop their
/// seed and publication commits.
fn rewrite_direct_publication(
    unit: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    direct: &crate::HashSet<AbsoluteAddr>,
) {
    let staged = |address: &RegionedAbsoluteAddr| {
        matches!(address.region, WORKING_REGION | SPARSE_WORKING_REGION)
            && direct.contains(&address.absolute_addr())
    };
    for block in unit.blocks.values_mut() {
        block
            .instructions
            .retain_mut(|instruction| match instruction {
                SIRInstruction::Commit(source, destination, ..) => {
                    !(direct.contains(&source.absolute_addr())
                        && source.absolute_addr() == destination.absolute_addr()
                        && (staged(source) || staged(destination)))
                }
                SIRInstruction::Load(_, address, ..) | SIRInstruction::Store(address, ..) => {
                    if staged(address) {
                        address.region = STABLE_REGION;
                    }
                    true
                }
                _ => true,
            });
    }
}

/// Apply the per-unit stages of the late combinational pipeline, in the same
/// order as the sequential kernel, to every partitioned comb unit.
pub(super) fn optimize_late_parallel_comb(
    program: &mut OptimizationContext<'_>,
    opt: &crate::OptimizeOptions,
    options: &PassOptions,
) {
    if program
        .sir
        .parallel
        .as_ref()
        .is_none_or(|parallel| parallel.eval_comb.is_empty())
    {
        return;
    }
    let on = |pass: SirPass| opt.is_enabled(pass);
    let packed_scatter_store =
        on(SirPass::PackedScatterStore).then(|| PackedScatterStorePass::for_program(program));
    let indexed_store_recovery =
        on(SirPass::IndexedStoreRecovery).then(|| IndexedStoreRecoveryPass::for_program(program));
    let sparse_case_dispatch = on(SirPass::SparseCaseDispatch)
        .then(|| SparseCaseDispatchPass::new(program.layout_requirements.state_aliases()));
    let parallel = program
        .sir
        .parallel
        .as_mut()
        .expect("checked parallel program above");
    for LaneUnit { unit, .. } in &mut parallel.eval_comb {
        let watermark = unit.blocks.keys().map(|block| block.0).max().unwrap_or(0);
        if on(SirPass::LoopIdiom) {
            pass_manager::ExecutionUnitPass::run(&LoopIdiomPass, unit, options);
        }
        if let Some(pass) = &packed_scatter_store {
            pass_manager::ExecutionUnitPass::run(pass, unit, options);
        }
        if let Some(pass) = &indexed_store_recovery {
            pass_manager::ExecutionUnitPass::run(pass, unit, options);
        }
        if on(SirPass::GuardedRegionSinking) {
            pass_manager::ExecutionUnitPass::run(&GuardedRegionSinkingPass, unit, options);
        }
        if let Some(pass) = &sparse_case_dispatch {
            pass_manager::ExecutionUnitPass::run(pass, unit, options);
        }
        if on(SirPass::BranchifyMux) {
            pass_branchify_mux::run_late_branchify_mux(unit, options, watermark);
            pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(unit);
            pass_manager::ExecutionUnitPass::run(&PhiOutcomeCompressionPass, unit, options);
        }
        if on(SirPass::Gvn) {
            pass_manager::ExecutionUnitPass::run(&GvnPass, unit, options);
        }
        if on(SirPass::ControlFlowSimplify) {
            pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, unit, options);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_sir::{LaneUnit, ParallelFfKernel};

    fn address(region: u32, var: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: StateObjectId::from_raw(var),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> LaneUnit<RegionedAbsoluteAddr> {
        let mut register_map = crate::HashMap::default();
        register_map.insert(RegisterId(0), RegisterType::Logic { width: 8 });
        LaneUnit::new(
            0,
            ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: Vec::new(),
                        instructions,
                        terminator: SIRTerminator::Return,
                    },
                )]
                .into_iter()
                .collect(),
                register_map,
            },
        )
    }

    fn commit(from: u32, to: u32, var: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Commit(
            address(from, var),
            address(to, var),
            SIROffset::Static(0),
            8,
            Vec::new(),
        )
    }

    fn store(region: u32, var: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(region, var),
            SIROffset::Static(0),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        )
    }

    fn load(region: u32, var: u32) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Load(RegisterId(0), address(region, var), SIROffset::Static(0), 8)
    }

    /// Two parts staging objects 0 and 1. The second evaluation reads
    /// object 0 when `later_reader` is set.
    fn kernel(later_reader: bool) -> ParallelFfKernel<RegionedAbsoluteAddr> {
        let mut second = vec![commit(STABLE_REGION, WORKING_REGION, 1)];
        if later_reader {
            second.push(load(STABLE_REGION, 0));
        }
        second.push(store(WORKING_REGION, 1));
        ParallelFfKernel {
            evaluations: vec![
                unit(vec![
                    commit(STABLE_REGION, WORKING_REGION, 0),
                    store(WORKING_REGION, 0),
                ]),
                unit(second),
            ],
            applications: vec![
                unit(vec![commit(WORKING_REGION, STABLE_REGION, 0)]),
                unit(vec![commit(WORKING_REGION, STABLE_REGION, 1)]),
            ],
        }
    }

    fn instructions(
        unit: &LaneUnit<RegionedAbsoluteAddr>,
    ) -> &[SIRInstruction<RegionedAbsoluteAddr>] {
        &unit.unit.blocks[&BlockId(0)].instructions
    }

    #[test]
    fn evaluations_publish_targets_no_later_unit_accesses() {
        let mut kernel = kernel(false);
        publish_kernel_directly(&mut kernel);
        for part in 0..2 {
            assert_eq!(
                instructions(&kernel.evaluations[part]),
                [store(STABLE_REGION, part as u32)]
            );
            assert!(instructions(&kernel.applications[part]).is_empty());
        }
    }

    #[test]
    fn a_target_read_by_a_later_evaluation_stays_staged() {
        let mut kernel = kernel(true);
        publish_kernel_directly(&mut kernel);
        assert_eq!(
            instructions(&kernel.evaluations[0]),
            [
                commit(STABLE_REGION, WORKING_REGION, 0),
                store(WORKING_REGION, 0)
            ]
        );
        assert_eq!(
            instructions(&kernel.applications[0]),
            [commit(WORKING_REGION, STABLE_REGION, 0)]
        );
        // The reader's own target is still published directly.
        assert_eq!(
            instructions(&kernel.evaluations[1]),
            [load(STABLE_REGION, 0), store(STABLE_REGION, 1)]
        );
        assert!(instructions(&kernel.applications[1]).is_empty());
    }
}
