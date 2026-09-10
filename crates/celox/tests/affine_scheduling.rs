#[path = "../examples/affine_support/mod.rs"]
mod support;

use celox::{InterpError, InterpMachine, ResolvedAccess, StoreSnapshot};
use celox_analysis::polyhedral::{Affine, ScheduleOptions, Tile, schedule};
use celox_design::RegionedStateAddr;
use celox_sir::{SIROffset, SIRValue, TriggerIdWithKind};
use num_traits::ToPrimitive;
use support::{Fixture, Kind, WORK};

#[derive(Clone)]
struct Machine(Vec<Vec<SIRValue>>);
fn index(access: ResolvedAccess<'_>) -> usize {
    match access.offset {
        SIROffset::Element {
            bit_offset: 0,
            dynamic_bit_offset: None,
            ..
        } => access.dynamics[0].unwrap().payload.to_usize().unwrap(),
        SIROffset::Static(bits) => bits / 64,
        SIROffset::Dynamic(_) => access.dynamics[0].unwrap().payload.to_usize().unwrap() / 64,
        _ => panic!("unexpected fixture offset"),
    }
}
impl InterpMachine<RegionedStateAddr> for Machine {
    fn load(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
    ) -> Result<SIRValue, InterpError> {
        assert_eq!(bits, 64);
        Ok(self.0[a.var_id.0 as usize][index(at)].clone())
    }
    fn store(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
        v: &SIRValue,
    ) -> Result<(), InterpError> {
        assert_eq!(bits, 64);
        self.0[a.var_id.0 as usize][index(at)] = v.clone();
        Ok(())
    }
    fn commit(
        &mut self,
        _: &RegionedStateAddr,
        _: &RegionedStateAddr,
        _: ResolvedAccess<'_>,
        _: usize,
    ) -> Result<(), InterpError> {
        panic!("commit in pure kernel")
    }
    fn notify_triggers(
        &mut self,
        _: &RegionedStateAddr,
        _: ResolvedAccess<'_>,
        _: usize,
        triggers: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        assert!(triggers.is_empty());
        Ok(())
    }
    fn notify_trigger_only_store(
        &mut self,
        _: &RegionedStateAddr,
        _: &[TriggerIdWithKind],
    ) -> Result<(), InterpError> {
        panic!("trigger in pure kernel")
    }
    fn capture_store_range(
        &mut self,
        _: &RegionedStateAddr,
        _: ResolvedAccess<'_>,
        _: usize,
    ) -> Result<StoreSnapshot, InterpError> {
        panic!("capture in pure kernel")
    }
    fn enable_comb_captures(
        &mut self,
        _: &RegionedStateAddr,
        _: ResolvedAccess<'_>,
        _: usize,
        _: &StoreSnapshot,
        _: &[u32],
    ) -> Result<(), InterpError> {
        panic!("capture in pure kernel")
    }
    fn emit_runtime_event(&mut self, _: u32, _: &[SIRValue]) -> Result<(), InterpError> {
        panic!("event in pure kernel")
    }
    fn emit_comb_capture_event(
        &mut self,
        _: u32,
        _: &[SIRValue],
        _: Option<i64>,
        _: bool,
    ) -> Result<(), InterpError> {
        panic!("event in pure kernel")
    }
    fn enable_comb_capture_if_changed(
        &mut self,
        _: &SIRValue,
        _: &SIRValue,
        _: &[u32],
    ) -> Result<(), InterpError> {
        panic!("capture in pure kernel")
    }
}

fn reference(fixture: &Fixture, mut cells: Vec<Vec<SIRValue>>) -> Vec<Vec<SIRValue>> {
    let mut data = cells
        .iter()
        .map(|a| {
            a.iter()
                .map(|v| v.payload.to_u64().unwrap())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    match fixture.kind {
        Kind::Shift => {
            let (input, rest) = data.split_first_mut().unwrap();
            for (output, &input) in rest[0].iter_mut().zip(input.iter()) {
                *output = input.wrapping_mul(3);
            }
            for i in 1..fixture.n - 1 {
                data[2][i] = data[1][i - 1]
                    .wrapping_add(data[1][i])
                    .wrapping_add(data[1][i + 1]);
            }
        }
        Kind::Jacobi => {
            for _ in 0..fixture.steps {
                for i in 1..fixture.n - 1 {
                    data[1][i] = data[0][i - 1]
                        .wrapping_add(data[0][i])
                        .wrapping_add(data[0][i + 1]);
                }
                let (a, b) = data.split_at_mut(1);
                a[0][1..fixture.n - 1].copy_from_slice(&b[0][1..fixture.n - 1]);
            }
        }
    }
    for (cells, data) in cells.iter_mut().zip(data) {
        for (cell, value) in cells.iter_mut().zip(data) {
            *cell = SIRValue::new(value);
        }
    }
    cells
}

#[test]
fn slt_and_cfg_schedules_preserve_memory_and_unknown_bits() {
    for kind in [Kind::Shift, Kind::Jacobi] {
        for n in [5, 19] {
            for four_state in [false, true] {
                let fixture = Fixture::new(kind, n, 3);
                let original = fixture.original(four_state);
                let initial = fixture.initial(four_state, n as u64);
                let mut expected = Machine(initial.clone());
                celox::execute_unit(&original, &mut expected, &[], four_state).unwrap();
                if !four_state {
                    assert_eq!(expected.0, reference(&fixture, initial.clone()));
                }
                for kernel in [
                    fixture.slt_kernel(four_state),
                    celox_sir::affine::extract(&original, &fixture.objects).unwrap(),
                ] {
                    let scheduled = schedule(kernel.region(), &ScheduleOptions::default()).unwrap();
                    if matches!(kind, Kind::Jacobi) {
                        assert_eq!(
                            scheduled.rows[1],
                            vec![Affine::new(vec![2, 1], 0), Affine::new(vec![2, 1], 1)]
                        );
                    }
                    let sizes = if matches!(kind, Kind::Shift) {
                        vec![4]
                    } else {
                        vec![2, 5]
                    };
                    let tile = Tile { band: 0, sizes };
                    let mut units = vec![
                        kernel.lower_original(WORK).unwrap(),
                        kernel.lower(&scheduled, None, WORK).unwrap(),
                        kernel.lower(&scheduled, Some(&tile), WORK).unwrap(),
                    ];
                    for unroll in [2, 7] {
                        let options = celox_sir::affine::CodegenOptions {
                            unroll,
                            ..Default::default()
                        };
                        for tile in [None, Some(&tile)] {
                            units.push(
                                kernel
                                    .lower_with_options(&scheduled, tile, &options, WORK)
                                    .unwrap(),
                            );
                        }
                    }
                    for unit in units {
                        let mut actual = Machine(initial.clone());
                        celox::execute_unit(&unit, &mut actual, &[], four_state).unwrap();
                        assert_eq!(
                            actual.0, expected.0,
                            "{kind:?}, n={n}, four_state={four_state}"
                        );
                        #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
                        if n == 19 {
                            let layout = fixture.layout(four_state);
                            let mut native = support::native::Executable::new(&unit, &layout);
                            assert!(native.code_bytes > 0);
                            native.initialize(&layout, &initial);
                            native.run();
                            assert_eq!(
                                native.cells(&layout, fixture.objects.len(), n),
                                expected.0,
                                "native {kind:?}, four_state={four_state}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn constrained_statement_endpoints_preserve_integer_rounding() {
    for four_state in [false, true] {
        let mut fixture = Fixture::new(Kind::Shift, 19, 1);
        fixture.statements[0].domain.constraints =
            vec![Affine::new(vec![2], -3), Affine::new(vec![-3], 22)];
        fixture.statements[1].domain.constraints =
            vec![Affine::new(vec![3], -14), Affine::new(vec![-2], 19)];
        let kernel = fixture.slt_kernel(four_state);
        let chosen = schedule(kernel.region(), &ScheduleOptions::default()).unwrap();
        // Independently rounded source intervals: [ceil(3/2), floor(22/3)]
        // and [ceil(14/3), floor(19/2)]. The direct CFG ignores constraints.
        fixture.statements[0].domain.bounds[0] = 2..8;
        fixture.statements[1].domain.bounds[0] = 5..10;
        let initial = fixture.initial(four_state, 123);
        let mut expected = Machine(initial.clone());
        celox::execute_unit(
            &fixture.original(four_state),
            &mut expected,
            &[],
            four_state,
        )
        .unwrap();
        let unit = kernel.lower(&chosen, None, WORK).unwrap();
        let mut actual = Machine(initial.clone());
        celox::execute_unit(&unit, &mut actual, &[], four_state).unwrap();
        assert_eq!(actual.0, expected.0);
        #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
        {
            let layout = fixture.layout(four_state);
            let mut native = support::native::Executable::new(&unit, &layout);
            native.initialize(&layout, &initial);
            native.run();
            assert_eq!(
                native.cells(&layout, fixture.objects.len(), fixture.n),
                expected.0
            );
        }
    }
}

#[test]
fn negative_time_tiles_and_disjoint_statement_interiors_preserve_results() {
    for kind in [Kind::Shift, Kind::Jacobi] {
        let mut fixture = Fixture::new(kind, 19, 3);
        if matches!(kind, Kind::Shift) {
            fixture.statements[0].domain.bounds[0] = 0..4;
            fixture.statements[1].domain.bounds[0] = 9..12;
        } else {
            for statement in &mut fixture.statements {
                statement.domain.bounds[0] = -2..1;
            }
        }
        let initial = fixture.initial(true, 999);
        // The Jacobi body does not use time in data computations, so shifting
        // all three original steps by -2 leaves this independent oracle valid.
        let mut expected = Machine(initial.clone());
        celox::execute_unit(&fixture.original(true), &mut expected, &[], true).unwrap();
        let kernel = fixture.slt_kernel(true);
        let chosen = schedule(kernel.region(), &ScheduleOptions::default()).unwrap();
        for size in [1, 4, 23] {
            let tile = Tile {
                band: 0,
                sizes: vec![size; chosen.bands[0].len()],
            };
            let options = celox_sir::affine::CodegenOptions {
                unroll: 7,
                ..Default::default()
            };
            let unit = kernel
                .lower_with_options(&chosen, Some(&tile), &options, WORK)
                .unwrap();
            let mut actual = Machine(initial.clone());
            celox::execute_unit(&unit, &mut actual, &[], true).unwrap();
            assert_eq!(actual.0, expected.0, "{kind:?}, tile={size}");
            #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
            {
                let layout = fixture.layout(true);
                let mut native = support::native::Executable::new(&unit, &layout);
                native.initialize(&layout, &initial);
                native.run();
                assert_eq!(
                    native.cells(&layout, fixture.objects.len(), fixture.n),
                    expected.0
                );
            }
        }
    }
}

#[test]
fn cfg_extraction_rejects_carried_state_events_and_indirect_addresses() {
    use celox_sir::{RegisterId, SIRInstruction, SIRTerminator};
    let fixture = Fixture::new(Kind::Jacobi, 19, 3);
    let original = fixture.original(false);
    let extract = |unit| celox_sir::affine::extract(unit, &fixture.objects);
    assert!(extract(&original).is_ok());
    let mut carried = original.clone();
    let (&header, block) = carried
        .blocks
        .iter()
        .find(|(_, block)| !block.params.is_empty())
        .unwrap();
    let ty = carried.register_map[&block.params[0]].clone();
    let extra = RegisterId(carried.register_map.keys().map(|r| r.0).max().unwrap() + 1);
    carried.register_map.insert(extra, ty);
    carried.blocks.get_mut(&header).unwrap().params.push(extra);
    for block in carried.blocks.values_mut() {
        if let SIRTerminator::Jump(target, args) = &mut block.terminator
            && *target == header
        {
            args.push(args[0]);
        }
    }
    carried.verify_result().unwrap();
    assert!(extract(&carried).is_err());
    let mut event = original.clone();
    event
        .blocks
        .get_mut(&event.entry_block_id)
        .unwrap()
        .instructions
        .push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![],
        });
    event.verify_result().unwrap();
    assert!(extract(&event).is_err());
    let mut indirect = original.clone();
    for block in indirect.blocks.values_mut() {
        if let Some(SIRInstruction::Store(_, offset, _, value, _, _)) = block
            .instructions
            .iter_mut()
            .find(|inst| matches!(inst, SIRInstruction::Store(..)))
        {
            *offset = SIROffset::Element {
                index: *value,
                element_width: 64,
                bit_offset: 0,
                dynamic_bit_offset: None,
            };
            break;
        }
    }
    indirect.verify_result().unwrap();
    assert!(extract(&indirect).is_err());
}
