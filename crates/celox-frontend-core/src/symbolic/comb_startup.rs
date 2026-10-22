//! Declaration-time sensitivity snapshots for wildcard processes.

use crate::{HashMap, SourceAddr};
use celox_design::{InitialStateData, InitialStateValue, VariableMetadata};
use celox_slt::CombStartup;
use num_bigint::BigUint;

pub(super) fn initial_values<'a>(
    startups: impl Iterator<Item = &'a CombStartup<SourceAddr>>,
    declarations: &[InitialStateValue<SourceAddr>],
    objects: &HashMap<SourceAddr, VariableMetadata>,
    four_state: bool,
) -> Vec<InitialStateValue<SourceAddr>> {
    let mut by_address: HashMap<_, Vec<_>> = HashMap::default();
    for initial in declarations {
        by_address
            .entry(initial.address)
            .or_default()
            .push(&initial.data);
    }
    let mut result = Vec::new();
    for startup in startups {
        result.push(InitialStateValue {
            address: startup.initialized,
            data: InitialStateData::Packed {
                value: 1u8.into(),
                mask: 0u8.into(),
                written_mask: 1u8.into(),
            },
        });
        for (source, saved) in &startup.inputs {
            let width = source.access.msb - source.access.lsb + 1;
            let all = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let mut value = BigUint::default();
            let mut mask = BigUint::default();
            let source_four_state = four_state && objects[&source.id].is_4state;
            for data in by_address.get(&source.id).into_iter().flatten() {
                match data {
                    InitialStateData::Packed {
                        value: incoming_value,
                        mask: incoming_mask,
                        written_mask,
                    } => {
                        let written = (written_mask >> source.access.lsb) & &all;
                        let mut incoming_value = (incoming_value >> source.access.lsb) & &written;
                        let mut incoming_mask = (incoming_mask >> source.access.lsb) & &written;
                        if !source_four_state {
                            incoming_value &= &all ^ &incoming_mask;
                            incoming_mask = BigUint::default();
                        }
                        value = (value & (&all ^ &written)) | incoming_value;
                        mask = (mask & (&all ^ &written)) | incoming_mask;
                    }
                    InitialStateData::Writes(runs) => {
                        for run in runs {
                            let lo = source.access.lsb.max(run.bit_offset);
                            let hi = (source.access.msb + 1).min(run.bit_offset + run.bit_width);
                            if lo >= hi {
                                continue;
                            }
                            let bits = (BigUint::from(1u8) << (hi - lo)) - BigUint::from(1u8);
                            let shift = lo - source.access.lsb;
                            let written = &bits << shift;
                            let mut incoming_value = (BigUint::from_bytes_le(&run.value_bytes)
                                >> (lo - run.bit_offset))
                                & &bits;
                            let mut incoming_mask = (BigUint::from_bytes_le(&run.mask_bytes)
                                >> (lo - run.bit_offset))
                                & &bits;
                            if !source_four_state {
                                incoming_value &= &bits ^ &incoming_mask;
                                incoming_mask = BigUint::default();
                            }
                            value = (value & (&all ^ &written)) | (incoming_value << shift);
                            mask = (mask & (&all ^ &written)) | (incoming_mask << shift);
                        }
                    }
                }
            }
            result.push(InitialStateValue {
                address: *saved,
                data: InitialStateData::Packed {
                    value,
                    mask,
                    written_mask: all,
                },
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceVarId;
    use celox_design::{DomainKind, InitialStateWriteRun, InstanceId, PortTypeKind, VarAtomBase};

    #[test]
    fn snapshots_select_logical_bits_after_ordered_partial_initializers() {
        let address = |id| SourceAddr {
            instance_id: InstanceId(0),
            var_id: SourceVarId(id),
        };
        let source = address(0);
        let startup = CombStartup {
            counters: Vec::new(),
            initialized: address(1),
            inputs: vec![(VarAtomBase::new(source, 4, 11), address(2))],
        };
        let objects = [(
            source,
            VariableMetadata {
                width: 16,
                is_4state: true,
                kind: DomainKind::Other,
                type_kind: PortTypeKind::Logic,
                array_dims: vec![2],
                value_kind: Default::default(),
            },
        )]
        .into_iter()
        .collect();
        let declarations = [
            InitialStateValue {
                address: source,
                data: InitialStateData::Packed {
                    value: 0xffffu32.into(),
                    mask: 0xffffu32.into(),
                    written_mask: 0xffffu32.into(),
                },
            },
            InitialStateValue {
                address: source,
                data: InitialStateData::Writes(vec![InitialStateWriteRun {
                    bit_offset: 8,
                    bit_width: 8,
                    value_bytes: vec![0xa5],
                    mask_bytes: vec![0],
                }]),
            },
        ];
        for (four_state, expected_value, expected_mask) in
            [(true, 0x5fu32, 0x0fu32), (false, 0x50, 0)]
        {
            let values = initial_values(
                std::iter::once(&startup),
                &declarations,
                &objects,
                four_state,
            );
            assert_eq!(
                values[0].data,
                InitialStateData::Packed {
                    value: 1u8.into(),
                    mask: 0u8.into(),
                    written_mask: 1u8.into(),
                }
            );
            assert_eq!(values[1].address, address(2));
            assert_eq!(
                values[1].data,
                InitialStateData::Packed {
                    value: expected_value.into(),
                    mask: expected_mask.into(),
                    written_mask: 0xffu32.into(),
                }
            );
        }
    }
}
