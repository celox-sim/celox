mod shifts;
mod strided;

use super::*;
use crate::{AbsoluteAddr, SIRValue};
use celox_design::{InstanceId, StateObjectId};
use celox_sir::BlockId as SirBlockId;
use celox_state_layout::MemoryLayoutMode;

fn layout_for(address: AbsoluteAddr, width: usize, four_state: bool) -> MemoryLayout {
    let plane_size = width.div_ceil(8);
    let total_size = plane_size * if four_state { 2 } else { 1 };
    MemoryLayout {
        trace: None,
        four_state,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: [(address, 0)].into_iter().collect(),
        widths: [(address, width)].into_iter().collect(),
        is_4states: [(address, four_state)].into_iter().collect(),
        total_size,
        working_offsets: HashMap::default(),
        working_base_offset: total_size,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: total_size,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: total_size,
        sparse_active_capacity: 0,
        merged_total_size: total_size,
        triggered_bits_offset: total_size,
        triggered_bits_total_size: 0,
        scratch_base_offset: total_size,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: Vec::new(),
    }
}

fn scalar_store_unit(
    address: RegionedAbsoluteAddr,
    four_state: bool,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let lhs = RegisterId(0);
    let rhs = RegisterId(1);
    let result = RegisterId(2);
    let value = if four_state {
        SIRValue::new_four_state(2u8, 1u8)
    } else {
        SIRValue::new(2u8)
    };
    ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Imm(lhs, value),
                    SIRInstruction::Imm(rhs, SIRValue::new(3u8)),
                    SIRInstruction::Binary(result, lhs, BinaryOp::Xor, rhs),
                    SIRInstruction::Store(
                        address,
                        SIROffset::Static(0),
                        8,
                        result,
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [lhs, rhs, result]
            .into_iter()
            .map(|register| {
                (
                    register,
                    if four_state {
                        RegisterType::Logic { width: 8 }
                    } else {
                        RegisterType::Bit {
                            width: 8,
                            signed: false,
                        }
                    },
                )
            })
            .collect(),
    }
}

#[test]
fn direct_pipeline_emits_two_state_aarch64_code() {
    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId::default(),
    };
    let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let layout = layout_for(absolute, 8, false);
    let unit = scalar_store_unit(address, false);

    let result =
        crate::scalar::emit_prepared_eu(&unit, &layout, false, "eval_comb", false, None, || false)
            .unwrap();

    assert!(!result.code.is_empty());
    assert_eq!(result.text_size % 4, 0);

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        let jit = crate::jit_mem::JitCode::new(&result.code).unwrap();
        let mut state = vec![0; layout.merged_total_size];
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state, [1]);
    }
}

#[test]
fn direct_pipeline_emits_four_state_aarch64_code() {
    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId::default(),
    };
    let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let layout = layout_for(absolute, 8, true);
    let unit = scalar_store_unit(address, true);

    let result =
        crate::scalar::emit_prepared_eu(&unit, &layout, true, "eval_comb", false, None, || false)
            .unwrap();

    assert!(!result.code.is_empty());
    assert_eq!(result.text_size % 4, 0);

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        let jit = crate::jit_mem::JitCode::new(&result.code).unwrap();
        let mut state = vec![0; layout.merged_total_size];
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        assert_eq!(state, [1, 1]);
    }
}
