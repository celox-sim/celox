use super::*;
use crate::ir::{
    BasicBlock, BlockId, InstanceId, RegisterId, RegisterType, SIRTerminator, StateObjectId,
};
use crate::{HashMap, HashSet};
use celox_state_layout::{
    LaneWriters, LayoutInput, LayoutRequirements, LayoutSource, MemoryLayoutMode, StateObjectLayout,
};

fn address(id: u32) -> AbsoluteAddr {
    AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    }
}

fn stable(id: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address(id))
}

struct Source {
    aliases: Vec<(u32, u32)>,
    width: usize,
}

impl LayoutSource<AbsoluteAddr> for Source {
    fn layout_input(&self, _mode: MemoryLayoutMode) -> LayoutInput<AbsoluteAddr> {
        let mut requirements = LayoutRequirements::default();
        for &(alias, canonical) in &self.aliases {
            requirements
                .state_aliases_mut()
                .insert(address(alias), address(canonical));
        }
        LayoutInput {
            state_objects: (0..4)
                .map(|id| StateObjectLayout {
                    address: address(id),
                    width: self.width,
                    is_4state: false,
                })
                .collect(),
            working_addresses: Vec::new(),
            sparse_addresses: Vec::new(),
            unpacked_arrays: HashMap::default(),
            requirements,
            ff_referenced_addresses: HashSet::default(),
            num_events: 0,
            runtime_event_sites: Vec::new(),
            lane_writers: LaneWriters::default(),
            access_rank: Default::default(),
            call_stack: None,
        }
    }
}

fn unit(
    lane: u32,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) -> LaneUnit<RegionedAbsoluteAddr> {
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        },
    );
    let register_map = (0..4)
        .map(|id| {
            (
                RegisterId(id),
                RegisterType::Bit {
                    width: 16,
                    signed: false,
                },
            )
        })
        .collect();
    LaneUnit::new(
        lane,
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        },
    )
}

fn load(id: u32, bit: usize, width: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Load(RegisterId(0), stable(id), SIROffset::Static(bit), width)
}

fn store(id: u32, bit: usize, width: usize) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Store(
        stable(id),
        SIROffset::Static(bit),
        width,
        RegisterId(0),
        Vec::new(),
        Vec::new(),
    )
}

fn layout(aliases: Vec<(u32, u32)>) -> MemoryLayout<AbsoluteAddr> {
    layout_with_width(aliases, 16)
}

fn layout_with_width(aliases: Vec<(u32, u32)>, width: usize) -> MemoryLayout<AbsoluteAddr> {
    MemoryLayout::build(&Source { aliases, width }, false, MemoryLayoutMode::Packed)
}

#[test]
fn many_producers_publish_only_the_latest_needed_by_each_consumer() {
    let mut lanes = vec![0; 100];
    lanes.push(1);
    let mut predecessors = vec![vec![]; 101];
    predecessors[100] = (0..100).collect();
    for earlier_consumer in [false, true] {
        if earlier_consumer {
            lanes.push(2);
            predecessors.push(vec![0]);
        }
        let publish = publication_boundaries(&lanes, &predecessors);
        assert_eq!(
            publish.iter().filter(|&&p| p).count(),
            if earlier_consumer { 2 } else { 1 }
        );
        assert!(publish[99]);
        assert_eq!(publish[0], earlier_consumer);
        let tasks = form_lane_tasks_with_options(
            3,
            &lanes,
            &predecessors,
            &[],
            TaskFormationOptions {
                publish_after: &publish,
                ..Default::default()
            },
        )
        .unwrap();
        verify_lane_tasks(3, &tasks, &predecessors).unwrap();
        assert_eq!(tasks.len(), if earlier_consumer { 4 } else { 2 });
    }
}

#[test]
fn reads_and_writes_of_one_home_are_ordered_both_ways() {
    let units = [
        unit(0, vec![store(0, 0, 16)]),
        unit(1, vec![load(0, 0, 16)]),
        unit(1, vec![load(1, 0, 16)]),
        unit(0, vec![store(1, 0, 16)]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout(Vec::new()),
        CodegenFootprint::Native,
    );
    assert_eq!(dependencies[1], vec![0]); // read after write
    assert!(dependencies[2].is_empty());
    assert_eq!(dependencies[3], vec![2]); // write after read
}

#[test]
fn writes_are_ordered_by_their_read_modify_write_envelopes() {
    let units = [
        unit(0, vec![store(0, 0, 8)]),
        unit(1, vec![load(0, 8, 8)]),
        // A whole byte at byte 1: its envelope is disjoint from byte 0.
        unit(1, vec![store(0, 8, 8)]),
        // Twelve bits at bit 4 span two bytes; the access covers both.
        unit(2, vec![store(0, 4, 12)]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout(Vec::new()),
        CodegenFootprint::Native,
    );
    assert!(dependencies[1].is_empty());
    // Byte 1 is read by unit 1 before unit 2 writes it.
    assert_eq!(dependencies[2], vec![1]);
    // Unit 1's read is already ordered before unit 2's write.
    assert_eq!(dependencies[3], vec![0, 2]);
}

#[test]
fn reads_and_writes_of_disjoint_bits_in_one_byte_are_independent() {
    let units = [
        unit(0, vec![store(0, 0, 3)]),
        // Bits 3..8 share byte 0 with the store but not its bits.
        unit(1, vec![load(0, 3, 5)]),
        unit(1, vec![store(0, 4, 1)]),
        unit(2, vec![load(0, 2, 2)]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout(Vec::new()),
        CodegenFootprint::Native,
    );
    assert!(dependencies[1].is_empty());
    // Bit 4 is read by unit 1; both writes of byte 0 are ordered.
    assert_eq!(dependencies[2], vec![0, 1]);
    // Bit 2 is written by unit 0, bit 3 is untouched by unit 2.
    assert_eq!(dependencies[3], vec![0]);
}

#[test]
fn native_static_writes_round_their_last_part() {
    let span = |shift, width| {
        CodegenFootprint::Native.static_write_span(
            shift,
            width,
            WriteKind::Store { source_width: 64 },
        )
    };
    assert_eq!(span(0, 32), 4);
    assert_eq!(span(0, 24), 4);
    assert_eq!(span(4, 8), 2);
    assert_eq!(span(0, 40), 8);
    // Nine bytes: one 8-byte part and a 1-byte part.
    assert_eq!(span(4, 64), 9);
    // Thirteen bytes: an 8-byte part and a 5-byte tail rounded to 8.
    assert_eq!(span(0, 100), 16);
    assert_eq!(span(0, 128), 16);
}

#[test]
fn cranelift_static_writes_depend_on_register_and_object_width() {
    let store = |shift, width, source_width| {
        CodegenFootprint::Cranelift.static_write_span(
            shift,
            width,
            WriteKind::Store { source_width },
        )
    };
    assert_eq!(store(0, 32, 32), 4);
    assert_eq!(store(4, 8, 8), 2);
    // A narrow store from a wide register writes whole words.
    assert_eq!(store(0, 1, 128), 8);
    // A scalar crossing a word boundary writes two words.
    assert_eq!(store(4, 64, 64), 16);
    let commit = |shift, width, object_width| {
        CodegenFootprint::Cranelift.static_write_span(
            shift,
            width,
            WriteKind::Commit { object_width },
        )
    };
    assert_eq!(commit(4, 4, 64), 1);
    assert_eq!(commit(0, 16, 128), 2);
    assert_eq!(commit(0, 4, 100), 8);
}

#[test]
fn coalesced_odd_widths_are_ordered_against_neighbouring_writes() {
    // A 24-bit store [0, 3) may be written as a 4-byte access, so a
    // store to byte 3 in another lane must be ordered after it.
    let units = [
        unit(0, vec![store(0, 0, 24)]),
        unit(1, vec![store(1, 0, 16)]),
        unit(1, vec![store(0, 24, 8)]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout_with_width(Vec::new(), 32),
        CodegenFootprint::Native,
    );
    assert!(dependencies[1].is_empty());
    assert_eq!(dependencies[2], vec![0]);
}

#[test]
fn unpadded_strided_array_writes_round_across_elements() {
    // Four 16-bit elements with a 2-byte stride: a 48-bit store of
    // elements 0..3 is one 6-byte field written as an 8-byte access,
    // which reads and writes back element 3.
    let strided = |element_width, element_stride| {
        let mut layout = layout_with_width(Vec::new(), 64);
        layout.unpacked_arrays.insert(
            address(0),
            celox_state_layout::UnpackedArrayLayout {
                element_width,
                element_count: 4,
                element_stride,
                plane_size: 4 * element_stride,
            },
        );
        layout
    };
    let packed = |bit, width, element_width| {
        SIRInstruction::Store(
            stable(0),
            SIROffset::PackedElements {
                bit_offset: bit,
                element_width,
            },
            width,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        )
    };
    let units = [
        unit(0, vec![packed(0, 48, 16)]),
        unit(1, vec![packed(48, 16, 16)]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &strided(16, 2),
        CodegenFootprint::Native,
    );
    assert_eq!(dependencies[1], vec![0]);
    // Padded 12-bit elements are written one slot at a time.
    let units = [
        unit(0, vec![packed(0, 36, 12)]),
        unit(1, vec![packed(36, 12, 12)]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &strided(12, 2),
        CodegenFootprint::Native,
    );
    assert!(dependencies[1].is_empty());
}

#[test]
fn aliased_homes_are_compared_by_storage() {
    let layout = layout(vec![(1, 0)]);
    assert_eq!(layout.offsets[&address(0)], layout.offsets[&address(1)]);
    let units = [
        unit(0, vec![store(0, 0, 16)]),
        unit(1, vec![load(1, 0, 16)]),
    ];
    assert_eq!(
        unit_dependencies(
            &units.iter().collect::<Vec<_>>(),
            &layout,
            CodegenFootprint::Native
        )[1],
        vec![0]
    );
}

#[test]
fn runtime_events_are_serialized() {
    let event = || SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: Vec::new(),
    };
    let units = [
        unit(0, vec![event()]),
        unit(1, vec![load(2, 0, 16)]),
        unit(1, vec![event()]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout(Vec::new()),
        CodegenFootprint::Native,
    );
    assert!(dependencies[1].is_empty());
    assert_eq!(dependencies[2], vec![0]);
    let tasks = plan_parallel_kernel(
        &units.iter().collect::<Vec<_>>(),
        &layout(Vec::new()),
        2,
        CodegenFootprint::Native,
    )
    .unwrap();
    assert_eq!(tasks.len(), 3);
}

fn call(
    reads: Vec<RegionedAbsoluteAddr>,
    writes: Vec<RegionedAbsoluteAddr>,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: RegisterId(0),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: std::sync::Arc::new(celox_sir::CallEffects {
            reads,
            writes,
            ..Default::default()
        }),
    }
}

/// A call is ordered after the writes of the objects it reads and before
/// later accesses of those it writes, but not against other objects;
/// two calls are ordered by the call stack they share.
#[test]
fn a_call_depends_on_the_objects_its_effects_list_and_on_other_calls() {
    let mut layout = layout(Vec::new());
    layout.call_stack = Some(celox_state_layout::CallStackLayout {
        depth_offset: 4096,
        base_offset: 4104,
        size: 64,
    });
    let units = [
        unit(0, vec![store(0, 0, 16)]),
        unit(1, vec![store(1, 0, 16)]),
        unit(2, vec![call(vec![stable(0)], vec![stable(2)])]),
        unit(0, vec![load(2, 0, 16)]),
        unit(1, vec![load(1, 0, 16)]),
        unit(3, vec![call(Vec::new(), vec![stable(3)])]),
    ];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout,
        CodegenFootprint::Native,
    );
    assert_eq!(dependencies[2], vec![0]); // reads what unit 0 wrote
    assert_eq!(dependencies[3], vec![2]); // reads what the call wrote
    assert_eq!(dependencies[4], vec![1]); // the call leaves object 1 alone
    assert_eq!(dependencies[5], vec![2]); // the shared call stack
}

/// A call whose callee stores with comb-capture sites shares the capture
/// flags with a capture store elsewhere: its summary and the direct store
/// take that resource from the same `host_effects` query.
#[test]
fn a_call_to_a_capturing_callee_is_ordered_with_capture_stores() {
    let captured = SIRInstruction::Store(
        stable(6),
        SIROffset::Static(0),
        16,
        RegisterId(0),
        Vec::new(),
        vec![0],
    );
    let callee_host = captured.host_effects();
    let mut effects = celox_sir::CallEffects {
        writes: vec![stable(7)],
        ..Default::default()
    };
    effects.host = callee_host;
    let call = SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: RegisterId(0),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: std::sync::Arc::new(effects),
    };
    let mut layout = layout(Vec::new());
    layout.call_stack = Some(celox_state_layout::CallStackLayout {
        depth_offset: 4096,
        base_offset: 4104,
        size: 64,
    });
    let units = [unit(0, vec![captured]), unit(1, vec![call])];
    let dependencies = unit_dependencies(
        &units.iter().collect::<Vec<_>>(),
        &layout,
        CodegenFootprint::Native,
    );
    assert_eq!(dependencies[1], vec![0]);
}
