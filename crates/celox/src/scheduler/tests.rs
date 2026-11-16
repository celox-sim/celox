use super::*;

#[test]
fn compiler_clock_hints_preserve_the_image_encoding() {
    #[derive(serde::Serialize)]
    struct LegacyClock {
        clock: u32,
        gate: Option<Vec<celox_design::GateOp<u32>>>,
    }
    let legacy = LegacyClock {
        clock: 1,
        gate: None,
    };
    let clock = celox_design::CombDerivedClock {
        clock: legacy.clock,
        gate: legacy.gate.clone(),
        inputs: Some(vec![1, 2, 3]),
    };
    let bytes = postcard::to_allocvec(&clock).unwrap();
    assert_eq!(bytes, postcard::to_allocvec(&legacy).unwrap());
    let decoded: celox_design::CombDerivedClock<u32> = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.clock, clock.clock);
    assert!(decoded.inputs.is_none());
}

#[test]
fn retention_uses_logical_ranges_for_packed_elements() {
    let address = AbsoluteAddr {
        instance_id: celox_design::InstanceId(0),
        var_id: celox_design::StateObjectId(0),
    };
    let offsets = [
        SIROffset::Static(0),
        SIROffset::PackedElements {
            bit_offset: 0,
            element_width: 4,
        },
    ];
    for store in &offsets {
        for load in &offsets {
            for width in [8, 32] {
                let mut b = Builder::new();
                let value = b.alloc_bit(32, false);
                let read = b.alloc_bit(32, false);
                b.emit(SIRInstruction::Imm(value, SIRValue::new(0u32)));
                b.emit(SIRInstruction::Store(
                    stable(address),
                    store.clone(),
                    width,
                    value,
                    vec![],
                    vec![],
                ));
                b.emit(SIRInstruction::Load(
                    read,
                    stable(address),
                    load.clone(),
                    32,
                ));
                b.seal_block(SIRTerminator::Return);
                let (blocks, register_map, _) = b.drain();
                let unit = Unit {
                    entry_block_id: celox_sir::BlockId(0),
                    blocks,
                    register_map,
                };
                assert_eq!(
                    retains_state(&[unit], &|a| a, &HashSet::from_iter([address])),
                    width < 32,
                    "store={store:?} load={load:?} width={width}"
                );
            }
        }
    }
}
