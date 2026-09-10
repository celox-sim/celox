#[path = "../examples/affine_veryl_support/mod.rs"]
mod frontend;
#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
#[allow(dead_code)]
#[path = "../examples/affine_support/mod.rs"]
mod support;

use celox::{InterpError, InterpMachine, ResolvedAccess, StoreSnapshot};
use celox_analysis::polyhedral::{Tile, schedule};
use celox_design::RegionedStateAddr;
use celox_sir::{SIROffset, SIRValue, TriggerIdWithKind, affine::recover_independent_stores};
use fxhash::FxHashMap as HashMap;
use num_traits::ToPrimitive;

const WORK: usize = 2_000_000;

#[derive(Clone)]
struct Machine(HashMap<RegionedStateAddr, Vec<SIRValue>>);
fn index(access: ResolvedAccess<'_>) -> usize {
    match access.offset {
        SIROffset::Element {
            bit_offset: 0,
            dynamic_bit_offset: None,
            ..
        } => access.dynamics[0].unwrap().payload.to_usize().unwrap(),
        SIROffset::Static(bits) => bits / 32,
        SIROffset::Dynamic(_) => access.dynamics[0].unwrap().payload.to_usize().unwrap() / 32,
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
        assert_eq!(bits, 32);
        Ok(self.0[a][index(at)].clone())
    }
    fn store(
        &mut self,
        a: &RegionedStateAddr,
        at: ResolvedAccess<'_>,
        bits: usize,
        v: &SIRValue,
    ) -> Result<(), InterpError> {
        assert_eq!(bits, 32);
        self.0.get_mut(a).unwrap()[index(at)] = v.clone();
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

#[test]
fn real_frontend_regions_preserve_cells_masks_and_boundaries() {
    for n in [3, 7, 19] {
        let mut cases = frontend::cases(n).into_iter().take(2).collect::<Vec<_>>();
        cases.push((
            "late_lane",
            format!(
                r#"module Top (a: input logic<32>[{n}], y: output logic<32>[{n}]) {{
            always_comb {{
                for i in 0..{last} {{ y[i] = a[i] * 32'd3; }}
                y[{last}] = a[{before}] * 32'd5;
            }}
        }}"#,
                last = n - 1,
                before = n - 2
            ),
        ));
        for (name, code) in cases {
            for four_state in [false, true] {
                let (program, trace, _) = frontend::compile_mode(&code, four_state);
                let objects = frontend::objects(&program);
                let pre = &trace.pre_optimized_sir.as_ref().unwrap().sir.eval_comb[0];
                assert!(celox_sir::affine::extract(pre, &objects).is_err());
                let kernel =
                    recover_independent_stores(pre, &objects, &Default::default()).unwrap();
                let chosen = schedule(kernel.region(), &Default::default()).unwrap();
                let tile = Tile {
                    band: 0,
                    sizes: vec![4],
                };
                let mut units = vec![
                    kernel.lower_original(WORK).unwrap(),
                    kernel.lower(&chosen, None, WORK).unwrap(),
                    kernel.lower(&chosen, Some(&tile), WORK).unwrap(),
                ];
                // Also compare native baseline output with the interpreter.
                #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
                units.push(program.sir.eval_comb[0].clone());
                #[cfg(not(all(feature = "host-runtime", target_arch = "x86_64")))]
                let _ = &mut units;
                for (variant, unit) in units.iter().enumerate() {
                    #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
                    let (layout, mut native) = {
                        use celox_state_layout::{MemoryLayout, MemoryLayoutMode};
                        assert!(program.layout_requirements.is_empty());
                        let layout =
                            MemoryLayout::build(&program, four_state, MemoryLayoutMode::Packed);
                        let native =
                            (n == 19).then(|| support::native::Executable::new(unit, &layout));
                        (layout, native)
                    };
                    for unknown in [false, true]
                        .into_iter()
                        .filter(|&unknown| !unknown || four_state)
                    {
                        let initial: HashMap<_, _> = objects
                            .iter()
                            .filter(|(a, _)| a.region == 0)
                            .map(|(a, object)| {
                                let cells = (0..object.elements)
                                    .map(|i| {
                                        SIRValue::new_four_state(
                                            (i as u32)
                                                .wrapping_mul(2_654_435_761)
                                                .wrapping_add(0x1234_5678),
                                            if unknown {
                                                [0, u32::MAX, 0xaaaa_aaaa, 0x5555_5555][i % 4]
                                            } else {
                                                0
                                            },
                                        )
                                    })
                                    .collect();
                                (*a, cells)
                            })
                            .collect();
                        let mut expected = Machine(initial.clone());
                        celox::execute_unit(pre, &mut expected, &[], four_state).unwrap();
                        if !unknown {
                            let address = |name| {
                                RegionedStateAddr::from_absolute_addr(
                                    0,
                                    program.get_addr(&[], &[name]).unwrap(),
                                )
                            };
                            let a = &initial[&address("a")];
                            let b = a
                                .iter()
                                .map(|v| v.payload.to_u32().unwrap().wrapping_mul(3))
                                .collect::<Vec<_>>();
                            let mut y = b.clone();
                            if name == "producer_consumer" {
                                y[0] = 0;
                                y[n - 1] = 0;
                                for i in 1..n - 1 {
                                    y[i] = b[i - 1].wrapping_add(b[i]).wrapping_add(b[i + 1]);
                                }
                                assert_eq!(
                                    expected.0[&address("b")],
                                    b.into_iter().map(SIRValue::new).collect::<Vec<_>>()
                                );
                            } else if name == "late_lane" {
                                y[n - 1] = a[n - 2].payload.to_u32().unwrap().wrapping_mul(5);
                            }
                            assert_eq!(
                                expected.0[&address("y")],
                                y.into_iter().map(SIRValue::new).collect::<Vec<_>>()
                            );
                        }
                        if variant < 3 {
                            let mut actual = Machine(initial.clone());
                            celox::execute_unit(unit, &mut actual, &[], four_state).unwrap();
                            assert_eq!(
                                actual.0, expected.0,
                                "{name} n={n} four_state={four_state} unknown={unknown} variant={variant}"
                            );
                        }
                        #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
                        if let Some(native) = native.as_mut() {
                            let write = |state: &mut [u64], byte: usize, value: u32| {
                                let shift = 8 * (byte % 8);
                                state[byte / 8] = (state[byte / 8]
                                    & !(u64::from(u32::MAX) << shift))
                                    | (u64::from(value) << shift);
                            };
                            for (a, values) in &initial {
                                let base = layout.offsets[&a.absolute_addr()];
                                for (i, value) in values.iter().enumerate() {
                                    write(
                                        &mut native.state,
                                        base + i * 4,
                                        value.payload.to_u32().unwrap(),
                                    );
                                    if four_state {
                                        write(
                                            &mut native.state,
                                            base + (values.len() + i) * 4,
                                            value.mask.to_u32().unwrap(),
                                        );
                                    }
                                }
                            }
                            native.run();
                            let read =
                                |byte: usize| (native.state[byte / 8] >> (8 * (byte % 8))) as u32;
                            for (a, values) in &expected.0 {
                                let base = layout.offsets[&a.absolute_addr()];
                                for (i, value) in values.iter().enumerate() {
                                    assert_eq!(
                                        read(base + i * 4),
                                        value.payload.to_u32().unwrap(),
                                        "native payload {name}, variant={variant}"
                                    );
                                    if four_state {
                                        assert_eq!(
                                            read(base + (values.len() + i) * 4),
                                            value.mask.to_u32().unwrap(),
                                            "native mask {name}, variant={variant}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn frontend_scalar_recurrence_does_not_become_independent_stores() {
    let (_, code) = frontend::cases(19).remove(2);
    let (program, trace, _) = frontend::compile_mode(&code, false);
    let pre = &trace.pre_optimized_sir.as_ref().unwrap().sir.eval_comb[0];
    let objects = frontend::objects(&program);
    assert!(celox_sir::affine::extract(pre, &objects).is_err());
    assert!(recover_independent_stores(pre, &objects, &Default::default()).is_err());
}
