//! Shared fixtures for the opt-in affine experiment and its differential tests.

// A one-dimensional iteration domain is a vector containing one range.
#![allow(clippy::single_range_in_vec_init)]

use celox_analysis::polyhedral::{Affine, Domain};
use celox_design::{
    BitAccess, InstanceId, RegionedStateAddr, StateAddr, StateObjectId, VarAtomBase,
};
use celox_sir::{
    BinaryOp, BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue,
    affine::{Kernel, MemoryObject},
};
use celox_slt::{
    NodeId, SLTIndex, SLTIndexKind, SLTNode, SLTNodeArena, SLTToSIRLowerer,
    affine::{ArrayStatement, ArrayStore},
};
use celox_state_layout::{
    LayoutInput, LayoutRequirements, LayoutSource, MemoryLayoutMode, StateObjectLayout,
    UnpackedArrayLayout,
};
use fxhash::FxHashMap as HashMap;

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
use celox_state_layout::MemoryLayout;

pub const WORK: usize = 2_000_000;
pub const WIDTH: usize = 64;

pub fn address(id: u32) -> RegionedStateAddr {
    RegionedStateAddr {
        region: 0,
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Shift,
    Jacobi,
}

pub struct Fixture {
    pub kind: Kind,
    pub n: usize,
    pub steps: usize,
    pub arena: SLTNodeArena<RegionedStateAddr>,
    pub statements: Vec<ArrayStatement<RegionedStateAddr>>,
    pub objects: HashMap<RegionedStateAddr, MemoryObject>,
}

fn input(arena: &mut SLTNodeArena<RegionedStateAddr>, id: u32, index: Option<NodeId>) -> NodeId {
    arena
        .alloc(SLTNode::Input {
            variable: address(id),
            signed: index.is_none(),
            index: index
                .map(|node| SLTIndex {
                    node,
                    stride: WIDTH,
                    kind: SLTIndexKind::Unpacked {
                        element_width: WIDTH,
                    },
                })
                .into_iter()
                .collect(),
            access: BitAccess::new(0, WIDTH - 1),
        })
        .unwrap()
}

impl Fixture {
    pub fn new(kind: Kind, n: usize, steps: usize) -> Self {
        assert!(n >= 3 && steps > 0);
        let mut arena = SLTNodeArena::new();
        let i = input(&mut arena, 101, None);
        let one = arena
            .alloc(SLTNode::Constant(1u64.into(), 0u64.into(), WIDTH, true))
            .unwrap();
        let left = arena.alloc(SLTNode::Binary(i, BinaryOp::Sub, one)).unwrap();
        let right = arena.alloc(SLTNode::Binary(i, BinaryOp::Add, one)).unwrap();
        let array = if matches!(kind, Kind::Shift) { 1 } else { 0 };
        let a = input(&mut arena, array, Some(left));
        let b = input(&mut arena, array, Some(i));
        let c = input(&mut arena, array, Some(right));
        let sum = arena.alloc(SLTNode::Binary(a, BinaryOp::Add, b)).unwrap();
        let sum = arena.alloc(SLTNode::Binary(sum, BinaryOp::Add, c)).unwrap();
        let i_var = VarAtomBase::new(address(101), 0, 63);
        let statements = match kind {
            Kind::Shift => {
                let value = input(&mut arena, 0, Some(i));
                let three = arena
                    .alloc(SLTNode::Constant(3u64.into(), 0u64.into(), WIDTH, false))
                    .unwrap();
                let product = arena
                    .alloc(SLTNode::Binary(value, BinaryOp::Mul, three))
                    .unwrap();
                vec![
                    ArrayStatement {
                        domain: Domain::rectangular(vec![0..n as i64]),
                        original_schedule: vec![Affine::constant(1, 0), Affine::axis(1, 0)],
                        induction: vec![i_var],
                        stores: vec![ArrayStore {
                            address: address(1),
                            index: i,
                            value: product,
                        }],
                    },
                    ArrayStatement {
                        domain: Domain::rectangular(vec![1..n as i64 - 1]),
                        original_schedule: vec![Affine::constant(1, 1), Affine::axis(1, 0)],
                        induction: vec![i_var],
                        stores: vec![ArrayStore {
                            address: address(2),
                            index: i,
                            value: sum,
                        }],
                    },
                ]
            }
            Kind::Jacobi => {
                let copied = input(&mut arena, 1, Some(i));
                (0..2)
                    .map(|s| ArrayStatement {
                        domain: Domain::rectangular(vec![0..steps as i64, 1..n as i64 - 1]),
                        original_schedule: vec![
                            Affine::axis(2, 0),
                            Affine::constant(2, s),
                            Affine::axis(2, 1),
                        ],
                        induction: vec![VarAtomBase::new(address(100), 0, 63), i_var],
                        stores: vec![ArrayStore {
                            address: address(if s == 0 { 1 } else { 0 }),
                            index: i,
                            value: if s == 0 { sum } else { copied },
                        }],
                    })
                    .collect()
            }
        };
        let count = if matches!(kind, Kind::Shift) { 3 } else { 2 };
        let objects = (0..count)
            .map(|id| {
                (
                    address(id),
                    MemoryObject {
                        element_width: WIDTH,
                        elements: n,
                    },
                )
            })
            .collect();
        Self {
            kind,
            n,
            steps,
            arena,
            statements,
            objects,
        }
    }

    pub fn slt_kernel(&self, four_state: bool) -> Kernel<RegionedStateAddr> {
        celox_slt::affine::extract(&self.arena, &self.statements, &self.objects, four_state)
            .unwrap()
    }

    /// A direct canonical CFG baseline; it does not use the new scan generator.
    pub fn original(&self, four_state: bool) -> ExecutionUnit<RegionedStateAddr> {
        let widths = self
            .objects
            .iter()
            .map(|(a, o)| (*a, o.element_width))
            .collect();
        let lowerer =
            SLTToSIRLowerer::new(four_state).with_unpacked_input_types(&self.arena, &widths);
        let mut builder = SIRBuilder::new();
        let statements = |builder: &mut SIRBuilder<_>, time: Option<RegisterId>| {
            for statement in &self.statements {
                let bounds = statement.domain.bounds.last().unwrap();
                counted_loop(builder, bounds.start, bounds.end, |builder, i| {
                    let axes = time.into_iter().chain([i]);
                    let inputs: HashMap<_, _> =
                        statement.induction.iter().cloned().zip(axes).collect();
                    for store in &statement.stores {
                        let mut cache = HashMap::default();
                        let index = lowerer.lower_with_inputs(
                            builder,
                            store.index,
                            &self.arena,
                            &mut cache,
                            inputs.clone(),
                        );
                        let value = lowerer.lower_with_inputs(
                            builder,
                            store.value,
                            &self.arena,
                            &mut cache,
                            inputs.clone(),
                        );
                        builder.emit(SIRInstruction::Store(
                            store.address,
                            SIROffset::Element {
                                index,
                                element_width: WIDTH,
                                bit_offset: 0,
                                dynamic_bit_offset: None,
                            },
                            WIDTH,
                            value,
                            vec![],
                            vec![],
                        ));
                    }
                });
            }
        };
        match self.kind {
            Kind::Shift => statements(&mut builder, None),
            Kind::Jacobi => counted_loop(&mut builder, 0, self.steps as i64, |builder, t| {
                statements(builder, Some(t))
            }),
        }
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let unit = ExecutionUnit {
            blocks,
            register_map,
            entry_block_id: BlockId(0),
        };
        unit.verify_result().unwrap();
        unit
    }

    #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
    pub fn layout(&self, four_state: bool) -> MemoryLayout<StateAddr> {
        MemoryLayout::build(self, four_state, MemoryLayoutMode::ElementStrided)
    }

    pub fn initial(&self, four_state: bool, seed: u64) -> Vec<Vec<SIRValue>> {
        let mut random = seed;
        (0..self.objects.len())
            .map(|_| {
                (0..self.n)
                    .map(|_| {
                        random = random
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let mask = if four_state && random & 15 == 0 {
                            1u64 << (random >> 58)
                        } else {
                            0
                        };
                        SIRValue {
                            payload: random.into(),
                            mask: mask.into(),
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

pub fn counted_loop<A>(
    builder: &mut SIRBuilder<A>,
    start: i64,
    end: i64,
    body: impl FnOnce(&mut SIRBuilder<A>, RegisterId),
) {
    let initial = builder.alloc_bit(64, true);
    builder.emit(SIRInstruction::Imm(initial, SIRValue::new(start as u64)));
    let bound = builder.alloc_bit(64, true);
    builder.emit(SIRInstruction::Imm(bound, SIRValue::new(end as u64)));
    let i = builder.alloc_bit(64, true);
    let header = builder.new_block_with(vec![i]);
    let enter = builder.new_block();
    let exit = builder.new_block();
    builder.seal_block(SIRTerminator::Jump(header, vec![initial]));
    builder.switch_to_block(header);
    let cond = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Binary(cond, i, BinaryOp::LtS, bound));
    builder.seal_block(SIRTerminator::Branch {
        cond,
        true_block: (enter, vec![]),
        false_block: (exit, vec![]),
    });
    builder.switch_to_block(enter);
    body(builder, i);
    let one = builder.alloc_bit(64, true);
    builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
    let next = builder.alloc_bit(64, true);
    builder.emit(SIRInstruction::Binary(next, i, BinaryOp::Add, one));
    builder.seal_block(SIRTerminator::Jump(header, vec![next]));
    builder.switch_to_block(exit);
}

impl LayoutSource<StateAddr> for Fixture {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<StateAddr> {
        LayoutInput {
            state_objects: self
                .objects
                .iter()
                .map(|(a, o)| StateObjectLayout {
                    address: a.absolute_addr(),
                    width: o.elements * o.element_width,
                    is_4state: true,
                })
                .collect(),
            unpacked_arrays: self
                .objects
                .iter()
                .map(|(a, o)| {
                    (
                        a.absolute_addr(),
                        UnpackedArrayLayout {
                            element_width: WIDTH,
                            element_count: o.elements,
                            element_stride: 8,
                            plane_size: o.elements * 8,
                        },
                    )
                })
                .collect(),
            working_addresses: vec![],
            sparse_addresses: vec![],
            requirements: LayoutRequirements::default(),
            ff_referenced_addresses: Default::default(),
            num_events: 0,
            runtime_event_sites: vec![],
        }
    }
}

#[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
pub mod native {
    use super::*;
    use celox_backend_x86::native::{emit::emit_prepared_eu, jit_mem::JitCode};
    use num_traits::ToPrimitive;

    pub struct Executable {
        pub code: JitCode,
        pub state: Vec<u64>,
        pub code_bytes: usize,
    }

    impl Executable {
        pub fn new(
            unit: &ExecutionUnit<RegionedStateAddr>,
            layout: &MemoryLayout<StateAddr>,
        ) -> Self {
            let mut unit = unit.clone();
            let widths = layout
                .unpacked_arrays
                .iter()
                .map(|(a, o)| {
                    (
                        RegionedStateAddr::from_absolute_addr(0, *a),
                        o.element_width,
                    )
                })
                .collect();
            celox_sir_opt::optimizer::optimize_merged_chain(
                &mut unit,
                std::sync::Arc::new(widths),
                |_, _, _| true,
                layout.four_state,
                false,
                &Default::default(),
                || false,
            )
            .unwrap();
            let emitted = emit_prepared_eu(
                &unit,
                layout,
                layout.four_state,
                "affine_experiment",
                &Default::default(),
                None,
                || false,
            )
            .unwrap();
            Self {
                code: JitCode::new(&emitted.code).unwrap(),
                state: vec![0; (emitted.required_state_size as usize).div_ceil(8)],
                code_bytes: emitted.text_size,
            }
        }
        pub fn initialize(&mut self, layout: &MemoryLayout<StateAddr>, cells: &[Vec<SIRValue>]) {
            for (object, cells) in cells.iter().enumerate() {
                let base = layout.offsets[&address(object as u32).absolute_addr()] / 8;
                for (i, value) in cells.iter().enumerate() {
                    self.state[base + i] = value.payload.to_u64().unwrap();
                    if layout.four_state {
                        self.state[base + cells.len() + i] = value.mask.to_u64().unwrap();
                    }
                }
            }
        }
        pub fn run(&mut self) {
            // The backend computed required_state_size including scratch and
            // saved registers; Vec<u64> supplies the required alignment.
            assert_eq!(
                unsafe { (self.code.fn_ptr)(self.state.as_mut_ptr().cast()) },
                0
            );
        }
        pub fn cells(
            &self,
            layout: &MemoryLayout<StateAddr>,
            objects: usize,
            n: usize,
        ) -> Vec<Vec<SIRValue>> {
            (0..objects)
                .map(|object| {
                    let base = layout.offsets[&address(object as u32).absolute_addr()] / 8;
                    (0..n)
                        .map(|i| SIRValue {
                            payload: self.state[base + i].into(),
                            mask: if layout.four_state {
                                self.state[base + n + i]
                            } else {
                                0
                            }
                            .into(),
                        })
                        .collect()
                })
                .collect()
        }
    }
}
