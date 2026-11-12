use super::*;

/// Applying the runs to a target equals the packed merge: written bits
/// take the value, unwritten bits keep the target's.
#[test]
fn packed_write_runs_match_the_packed_merge() {
    let width = 203;
    let all: BigUint = (BigUint::from(1u8) << width) - 1u8;
    let pattern = |seed: u64| {
        (0..width).fold(BigUint::default(), |acc, bit| {
            let set = (seed.wrapping_mul(bit as u64 + 1) >> 7).is_multiple_of(3);
            if set {
                acc | (BigUint::from(1u8) << bit)
            } else {
                acc
            }
        })
    };
    let (value, mask, current) = (pattern(0x9e37), pattern(0x51ed), pattern(0x2545));
    let written_masks = [
        BigUint::default(),
        all.clone(),
        pattern(0x7f4a) | (BigUint::from(1u8) << (width + 3)),
        (BigUint::from(1u8) << 64) - 1u8,
    ];
    for written in &written_masks {
        for four_state in [false, true] {
            let runs =
                InitialStateData::packed_write_runs(&value, &mask, written, width, four_state);
            let (mut got_value, mut got_mask) = (current.clone(), BigUint::default());
            for run in &runs {
                let run_mask = ((BigUint::from(1u8) << run.bit_width) - 1u8) << run.bit_offset;
                let keep = &all ^ &run_mask;
                got_value = (got_value & &keep)
                    | (BigUint::from_bytes_le(&run.value_bytes) << run.bit_offset);
                got_mask = (got_mask & &keep)
                    | (BigUint::from_bytes_le(&run.mask_bytes) << run.bit_offset);
            }
            let written = written & &all;
            let keep = &all ^ &written;
            let (want_value, want_mask) = if four_state {
                ((&current & &keep) | (&value & &written), &mask & &written)
            } else {
                let known = &all ^ (&mask & &all);
                (
                    (&current & &keep) | (&value & &written & known),
                    BigUint::default(),
                )
            };
            assert_eq!(
                (got_value, got_mask),
                (want_value, want_mask),
                "{four_state}"
            );
            assert!(
                runs.windows(2)
                    .all(|pair| { pair[0].bit_offset + pair[0].bit_width < pair[1].bit_offset })
            );
        }
    }
}

#[test]
fn host_request_encoding_keeps_families_and_full_site_indices_distinct() {
    for kind in [
        HostRequestKind::Plusargs,
        HostRequestKind::FileIo,
        HostRequestKind::Vcd,
        HostRequestKind::Scan,
        HostRequestKind::Distribution,
        HostRequestKind::Process,
        HostRequestKind::Testbench,
        HostRequestKind::Format,
        HostRequestKind::System,
    ] {
        for site in [0, 1, u32::MAX] {
            assert_eq!(
                HostRequestKind::decode(kind.encode(site)),
                Some((kind, site))
            );
        }
    }
    for invalid in [0, u32::MAX as u64, 10u64 << 32, 1u64 << 63] {
        assert_eq!(HostRequestKind::decode(invalid), None);
    }
    assert_eq!(
        ProcessStatus::from_code(10),
        Some(ProcessStatus::HostRequest)
    );
    assert_eq!(ProcessStatus::from_code(11), Some(ProcessStatus::Exit));
}

#[test]
fn host_request_mapping_relocates_operands_and_results() {
    let value = |id| HostRequestValue {
        value_kind: ValueKind::StrRef,
        id,
        width: 17,
        signed: true,
        is_string: false,
        float_precision: None,
    };
    let site = HostRequestSite {
        process: SiteProcess::own(2),
        kind: HostRequestKind::Plusargs,
        operation: 1,
        args: vec![value(3u32)],
        results: vec![value(5)],
        strings: vec!["COUNT=%d".into()],
    };
    let relocated = site.map(|id| id + 10);
    assert_eq!(relocated.slots().collect::<Vec<_>>(), vec![13, 15]);
    assert_eq!(relocated.process, SiteProcess::own(2));
    assert_eq!(relocated.args[0].width, 17);
    assert!(relocated.results[0].signed);
    assert_eq!(relocated.args[0].value_kind, ValueKind::StrRef);
    assert_eq!(relocated.strings, site.strings);
}

#[test]
fn bit_access_splits_only_at_internal_boundaries() {
    let access = BitAccess::new(4, 11);
    let bounds = [0, 4, 7, 12, 20].into_iter().collect();

    assert_eq!(
        access.calculate_atoms(&bounds),
        vec![BitAccess::new(4, 6), BitAccess::new(7, 11)]
    );
}

#[test]
fn design_ids_and_addresses_have_stable_display() {
    let address = AbsoluteAddrBase {
        instance_id: InstanceId(42),
        var_id: 7,
    };

    assert_eq!(ModuleId(3).to_string(), "mod3");
    assert_eq!(InstanceId(42).to_string(), "inst42");
    assert_eq!(StateObjectId(7).to_string(), "state7");
    assert_eq!(address.to_string(), "AbsoluteAddr(inst42, 7)");
}

#[test]
fn regioned_address_round_trips_semantic_identity() {
    let address = AbsoluteAddrBase {
        instance_id: InstanceId(2),
        var_id: 9,
    };
    let regioned = RegionedAbsoluteAddrBase::from_absolute_addr(WORKING_REGION, address);

    assert_eq!(regioned.absolute_addr(), address);
    assert_eq!(regioned.region, WORKING_REGION);
}

#[test]
fn semantic_operator_contracts_are_source_independent() {
    assert!(BinaryOp::Add.is_commutative());
    assert!(!BinaryOp::Sub.is_commutative());
    assert_eq!(UnaryOp::LogicNot.result_width(128), 1);
    assert_eq!(UnaryOp::PopCount.result_width(128), 8);
}

#[test]
fn initial_state_and_runtime_error_schemas_accept_design_owned_ids() {
    let initial = InitialStateValue {
        address: AbsoluteAddrBase {
            instance_id: InstanceId(1),
            var_id: 7u32,
        },
        data: InitialStateData::Writes(vec![InitialStateWriteRun {
            bit_offset: 3,
            bit_width: 5,
            value_bytes: vec![0x15],
            mask_bytes: vec![0],
        }]),
    };
    let error = RuntimeErrorInfo {
        message: "failed".to_string(),
        signals: vec![initial.address],
    };
    let mut runtime = RuntimeSchema::default();
    runtime.runtime_errors.insert(1, error.clone());
    runtime.runtime_event_sites.push(RuntimeEventSite {
        kind: RuntimeEventKind::Report(Severity::Fatal),
        template: Some("failed".to_string()),
        sizing: DisplaySizing::Veryl,
        scope: None,
        location: None,
        assertion: None,
        deferred: None,
        timestamp_arg: false,
        time_format_arg: false,
        arg_widths: Vec::new(),
        arg_signed: Vec::new(),
        arg_is_string: Vec::new(),
        arg_float_precision: Vec::new(),
        arg_is_handle: Vec::new(),
    });
    runtime.comb_observers.push(RuntimeCombObserver {
        site_id: 0,
        activation_group: 0,
        sensitivity: vec![VarAtomBase {
            id: initial.address,
            access: BitAccess { lsb: 3, msb: 7 },
        }],
        occurrence_counters: Vec::new(),
        written_inputs: vec![initial.address],
    });
    runtime.testbench_read_roots.insert(initial.address);

    assert_eq!(error.signals, vec![initial.address]);
    assert!(matches!(initial.data, InitialStateData::Writes(_)));
    assert_eq!(runtime.runtime_errors[&1], error);
    assert_eq!(runtime.runtime_event_sites.len(), 1);
    assert_eq!(runtime.comb_observers[0].sensitivity[0].id, initial.address);
    assert!(runtime.testbench_read_roots.contains(&initial.address));
}

#[test]
fn variable_metadata_preserves_elaborated_shape_and_domain() {
    let metadata = VariableMetadata {
        width: 32,
        is_4state: true,
        kind: DomainKind::Other,
        type_kind: PortTypeKind::Logic,
        array_dims: vec![4],
        value_kind: Default::default(),
    };

    assert_eq!(metadata.width, 32);
    assert_eq!(metadata.array_dims, vec![4]);
}

#[test]
fn elaborated_design_uses_flat_addresses_and_canonical_event_topology() {
    let mut design = ElaboratedDesign::<u32>::default();
    design.state_objects.insert(
        10,
        VariableMetadata {
            width: 1,
            is_4state: false,
            kind: DomainKind::ClockPosedge,
            type_kind: PortTypeKind::Clock,
            array_dims: Vec::new(),
            value_kind: Default::default(),
        },
    );
    design.events.aliases.insert(11, 10);
    design.events.ordered_events.push(10);

    assert_eq!(design.events.canonical(11), 10);
    assert_eq!(design.events.canonical(12), 12);
    assert_eq!(design.events.len(), 1);
    assert!(!design.events.is_empty());
    assert_eq!(design.state_objects[&10].width, 1);
}
#[test]
fn alias_event_kinds_merge_clock_and_reset_by_polarity() {
    for (clock, reset) in [
        (DomainKind::ClockPosedge, DomainKind::ResetAsyncHigh),
        (DomainKind::ClockNegedge, DomainKind::ResetAsyncLow),
    ] {
        for roles in [[clock, reset], [reset, clock]] {
            let mut design = ElaboratedDesign::<u32>::default();
            for (address, kind) in [(10, DomainKind::Other), (11, roles[0]), (12, roles[1])] {
                design.state_objects.insert(
                    address,
                    VariableMetadata {
                        width: 1,
                        is_4state: true,
                        kind,
                        type_kind: PortTypeKind::Logic,
                        array_dims: Vec::new(),
                        value_kind: Default::default(),
                    },
                );
            }
            design.events.ordered_events = vec![10];
            design.events.aliases.insert(11, 10);
            design.events.aliases.insert(12, 10);
            assert_eq!(design.event_kinds()[&10], clock);
            assert_eq!(design.state_objects[&10].kind, DomainKind::Other);
        }
    }
}

#[test]
fn alias_event_kinds_are_separate_from_declaration_metadata() {
    let mut design = ElaboratedDesign::<u32>::default();
    for (address, kind) in [
        (10, DomainKind::Other),
        (11, DomainKind::ClockPosedge),
        (20, DomainKind::Other),
        (21, DomainKind::ClockNegedge),
        (30, DomainKind::Other),
        (31, DomainKind::ClockPosedge),
        (32, DomainKind::ClockNegedge),
    ] {
        design.state_objects.insert(
            address,
            VariableMetadata {
                width: 1,
                is_4state: false,
                kind,
                type_kind: PortTypeKind::Logic,
                array_dims: Vec::new(),
                value_kind: Default::default(),
            },
        );
    }
    design.events.ordered_events = vec![10, 20, 30];
    for (alias, canonical) in [(11, 10), (21, 20), (31, 30), (32, 30)] {
        design.events.aliases.insert(alias, canonical);
    }
    let kinds = design.event_kinds();
    assert_eq!(kinds[&10], DomainKind::ClockPosedge);
    assert_eq!(kinds[&20], DomainKind::ClockNegedge);
    assert_eq!(
        kinds[&30],
        DomainKind::Other,
        "conflicting aliases do not select one edge"
    );
    assert_eq!(design.state_objects[&10].kind, DomainKind::Other);
    assert_eq!(design.state_objects[&20].type_kind, PortTypeKind::Logic);
}
