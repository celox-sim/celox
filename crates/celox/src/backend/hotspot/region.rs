//! Native block bodies with an explicit value/mask frame. The interpreter
//! executes the original terminator, so branch arguments and cold successors
//! retain their existing parallel-transfer semantics.

use crate::backend::{
    MemoryLayout, SimulatorErrorCode, compile_cancel::CompileCancel, native::backend::HotspotCode,
};
use crate::{
    HashMap, SimulatorError, SimulatorOptions,
    interpreter::{InterpError, Registers},
    ir::{
        AbsoluteAddr, ExecutionUnit, FRAME_REGION, InstanceId, RegionedAbsoluteAddr, STABLE_REGION,
    },
};
use celox_sir::{
    BasicBlock, BlockId, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue,
};
use num_bigint::BigUint;
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: lazily computed use counts and CFG analysis of a unit; only the hotspot worker thread fills and reads them, the simulation thread just holds the Arc"
)]
use std::sync::OnceLock;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SimBackend;

    #[test]
    fn hotspot_chain_exports_backedge_values_and_preserves_parallel_arguments() {
        check_frame(false, false);
    }

    #[test]
    fn hotspot_frame_discards_dirty_padding_in_exported_planes() {
        check_frame(true, false);
    }

    #[test]
    fn hotspot_frame_notifies_only_semantic_state_writes() {
        check_frame(false, true);
    }

    #[test]
    fn hotspot_chain_keeps_invariants_when_exiting_to_an_inner_loop() {
        let sim = crate::SimulatorBuilder::<crate::Simulator>::new(
            "module Top (q: output logic<130>) { assign q = 130'd0; }",
            "Top",
        )
        .four_state(true)
        .build_interpreter()
        .unwrap();
        let mut b = celox_sir::SIRBuilder::<RegionedAbsoluteAddr>::new();
        let invariant = b.alloc_logic(130);
        let value = SIRValue::new_four_state(BigUint::from(1u8) << 129usize, 3u8);
        b.emit(SIRInstruction::Imm(invariant, value.clone()));
        let header = b.new_block();
        b.seal_block(SIRTerminator::Jump(header, vec![]));
        b.switch_to_block(header);
        for _ in 0..16 {
            let used = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(used, vec![invariant]));
        }
        let addr = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            *sim.backend.layout().offsets.keys().next().unwrap(),
        );
        b.emit(SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            130,
            invariant,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Jump(header, vec![]));
        let (blocks, register_map, _) = b.drain();
        let unit = Arc::new(ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        });
        let source = Arc::new(RegionSource::new(Arc::clone(&unit)));
        let options = SimulatorOptions {
            four_state: true,
            ..Default::default()
        };
        let code = BlockRequest::new(&source, BlockId(0))
            .unwrap()
            .compile(sim.backend.layout(), &options, &CompileCancel::new())
            .unwrap();
        let resumed = BlockRequest::new(&source, header)
            .unwrap()
            .compile(sim.backend.layout(), &options, &CompileCancel::new())
            .unwrap();
        let mut memory = vec![
            0;
            code.code
                .required_state_size
                .max(resumed.code.required_state_size)
                .div_ceil(8)
        ];
        for stale in [false, true] {
            let mut regs = Registers::new(&unit.register_map, true);
            if stale {
                regs.set(invariant, SIRValue::new(42u8));
            }
            code.run(&mut memory, &mut regs).unwrap();
            assert_eq!(regs.get(invariant).unwrap(), &value);
            // Reenter the body, without rerunning the preheader definition.
            resumed.run(&mut memory, &mut regs).unwrap();
        }
    }

    fn check_frame(dirty_outputs: bool, tracing: bool) {
        let sim = crate::SimulatorBuilder::<crate::Simulator>::new(
            "module Top (q: output logic<130>) { assign q = 130'd0; }",
            "Top",
        )
        .four_state(true)
        .build_interpreter()
        .unwrap();
        let mut layout = sim.backend.layout().clone();
        if tracing {
            layout.enable_trace();
        }
        let state_addr = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            *layout.offsets.keys().next().unwrap(),
        );
        let mut b = celox_sir::SIRBuilder::new();
        let left = b.alloc_logic(130);
        let right = b.alloc_logic(130);
        let p = b.alloc_logic(130);
        let q = b.alloc_logic(130);
        let next = b.new_block_with(vec![p, q]);
        // Dead definitions exercise frame pruning independently of native DCE.
        for _ in 0..16 {
            let dead = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(dead, vec![left]));
        }
        b.seal_block(SIRTerminator::Jump(next, vec![right, left]));
        b.switch_to_block(next);
        let out = b.alloc_logic(130);
        b.emit(SIRInstruction::Concat(out, vec![p]));
        if tracing {
            b.emit(SIRInstruction::Store(
                state_addr,
                SIROffset::Static(0),
                130,
                out,
                Vec::new(),
                Vec::new(),
            ));
        }
        b.seal_block(SIRTerminator::Jump(BlockId(0), vec![q, out]));
        let (mut blocks, register_map, _) = b.drain();
        blocks.get_mut(&BlockId(0)).unwrap().params = vec![left, right];
        let unit = Arc::new(ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        });
        let source = Arc::new(RegionSource::new(Arc::clone(&unit)));
        let request = BlockRequest::new(&source, BlockId(0)).unwrap();
        assert_eq!(request.tail, next);
        let options = SimulatorOptions {
            four_state: true,
            ..Default::default()
        };
        let code = request
            .compile(&layout, &options, &CompileCancel::new())
            .unwrap();
        assert_eq!(
            code.outputs.iter().map(|(reg, _)| *reg).collect::<Vec<_>>(),
            vec![q, out]
        );
        let mut memory = vec![0; code.code.required_state_size.div_ceil(8)];
        let mut regs = Registers::new(&unit.register_map, true);
        let frame_start = code
            .inputs
            .iter()
            .chain(&code.outputs)
            .map(|(_, slot)| slot.offset)
            .min()
            .unwrap();
        if dirty_outputs {
            // Model a frame reused after a different region. These output-only
            // slots are never cleared by input marshalling.
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(memory.as_mut_ptr().cast::<u8>(), memory.len() * 8)
            };
            for (reg, slot) in &code.outputs {
                assert!(!code.inputs.iter().any(|(input, _)| input == reg));
                bytes[slot.offset..slot.offset + slot.bytes * 2].fill(0xff);
            }
        }
        let mut a = SIRValue::new_four_state(BigUint::from(1u8) << 129usize, BigUint::from(3u8));
        let mut z = SIRValue::new_four_state(BigUint::from(5u8), BigUint::from(1u8) << 128usize);
        for _ in 0..64 {
            regs.set(left, a.clone());
            regs.set(right, z.clone());
            code.run(&mut memory, &mut regs).unwrap();
            assert_eq!(regs.get(q).unwrap(), &a);
            assert_eq!(regs.get(out).unwrap(), &z);
            if let Some(trace) = &layout.trace {
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(
                        memory.as_mut_ptr().cast::<u8>(),
                        memory.len() * 8,
                    )
                };
                let expected = layout.trace_notification_offsets(&state_addr).unwrap();
                for (offset, &byte) in bytes
                    .iter()
                    .enumerate()
                    .take(frame_start)
                    .skip(trace.flags_offset)
                {
                    assert_eq!(
                        byte,
                        u8::from(expected.contains(&offset)),
                        "unexpected trace write at byte {offset}"
                    );
                }
                let mut groups = Vec::new();
                trace.take(bytes, &mut groups);
                assert_eq!(
                    groups,
                    vec![
                        layout.region_base_offset(&state_addr)
                            / celox_state_layout::TRACE_GROUP_BYTES
                    ]
                );
            }
            // Swap the inputs to exercise both X/Z planes in each slot.
            a = regs.get(out).unwrap().clone();
            z = regs.get(q).unwrap().clone();
        }
    }
}

const MAX_BLOCK_INSTRUCTIONS: usize = 4096;
const MAX_FRAME_BYTES: usize = 256 * 1024;

pub(crate) struct RegionSource {
    unit: Arc<ExecutionUnit<RegionedAbsoluteAddr>>,
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: lazily computed use counts and CFG analysis of a unit; only the hotspot worker thread fills and reads them, the simulation thread just holds the Arc"
    )]
    uses: OnceLock<HashMap<RegisterId, usize>>,
    next_register: usize,
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: lazily computed use counts and CFG analysis of a unit; only the hotspot worker thread fills and reads them, the simulation thread just holds the Arc"
    )]
    analysis: OnceLock<Option<SourceAnalysis>>,
}

struct SourceAnalysis {
    cfg: celox_sir::cfg::SirCfg,
    definitions: HashMap<RegisterId, Option<BlockId>>,
}

impl RegionSource {
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: lazily computed use counts and CFG analysis of a unit; only the hotspot worker thread fills and reads them, the simulation thread just holds the Arc"
    )]
    pub(crate) fn new(unit: Arc<ExecutionUnit<RegionedAbsoluteAddr>>) -> Self {
        let next_register = unit
            .register_map
            .keys()
            .map(|reg| reg.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        Self {
            unit,
            uses: OnceLock::new(),
            next_register,
            analysis: OnceLock::new(),
        }
    }

    fn analysis(&self) -> Option<&SourceAnalysis> {
        self.analysis
            .get_or_init(|| {
                let cfg = celox_sir::cfg::SirCfg::analyze_forward_structure(&self.unit).ok()?;
                let mut definitions = HashMap::default();
                for block in self.unit.blocks.values() {
                    for reg in block.params.iter().copied().chain(
                        block
                            .instructions
                            .iter()
                            .filter_map(SIRInstruction::defined_register),
                    ) {
                        definitions
                            .entry(reg)
                            .and_modify(|def| *def = None)
                            .or_insert(Some(block.id));
                    }
                }
                Some(SourceAnalysis { cfg, definitions })
            })
            .as_ref()
    }

    fn uses(&self) -> &HashMap<RegisterId, usize> {
        self.uses.get_or_init(|| {
            let mut counts = HashMap::<RegisterId, usize>::default();
            for block in self.unit.blocks.values() {
                let mut count = |reg| {
                    let total = counts.entry(reg).or_default();
                    *total = total.saturating_add(1);
                };
                for inst in &block.instructions {
                    inst.for_each_use(&mut count);
                }
                block.terminator.for_each_use(&mut count);
            }
            // A consumed use can run again after an exit into an inner loop,
            // without reexecuting a definition in the preheader. Preserve
            // cross-block values used in cyclic blocks, even when every
            // static use was consumed by this region. Same-block SSA uses
            // are preceded by their definition on each reentry.
            if let Some(analysis) = self.analysis() {
                for component in analysis.cfg.sccs.iter().filter(|scc| scc.cyclic) {
                    for &index in &component.blocks {
                        let id = analysis.cfg.block_ids[index];
                        let block = &self.unit.blocks[&id];
                        let mut retain = |reg| {
                            if analysis.definitions.get(&reg) != Some(&Some(id)) {
                                counts.insert(reg, usize::MAX);
                            }
                        };
                        for inst in &block.instructions {
                            inst.for_each_use(&mut retain);
                        }
                        block.terminator.for_each_use(&mut retain);
                    }
                }
            } else {
                // Unknown control flow must only make the frame larger.
                counts.values_mut().for_each(|count| *count = usize::MAX);
            }
            counts
        })
    }
}

pub(super) struct BlockRequest {
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    types: HashMap<RegisterId, RegisterType>,
    inputs: BTreeSet<RegisterId>,
    outputs: BTreeSet<RegisterId>,
    consumed: HashMap<RegisterId, usize>,
    source: Arc<RegionSource>,
    tail: BlockId,
}

fn eligible(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    match inst {
        SIRInstruction::Call { .. }
        | SIRInstruction::ExternCall { .. }
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. }
        | SIRInstruction::Commit(..) => false,
        SIRInstruction::Store(addr, _, _, _, triggers, captures) => {
            addr.region != FRAME_REGION && triggers.is_empty() && captures.is_empty()
        }
        SIRInstruction::Load(_, addr, _, _) => addr.region != FRAME_REGION,
        _ => true,
    }
}

impl BlockRequest {
    pub(super) fn new(source: &Arc<RegionSource>, start: BlockId) -> Option<Self> {
        let unit = &source.unit;
        let mut instructions = Vec::new();
        let mut consumed = HashMap::<RegisterId, usize>::default();
        let mut extra_types = HashMap::default();
        let mut next_register = source.next_register;
        let mut current = start;
        let mut seen = BTreeSet::new();
        loop {
            let block = unit.blocks.get(&current)?;
            if !block.instructions.iter().all(eligible)
                || instructions.len() + block.instructions.len() > MAX_BLOCK_INSTRUCTIONS
            {
                return None;
            }
            seen.insert(current);
            for inst in &block.instructions {
                inst.for_each_use(|reg| {
                    *consumed.entry(reg).or_default() += 1;
                });
                instructions.push(inst.clone());
            }
            let SIRTerminator::Jump(target, args) = &block.terminator else {
                break;
            };
            let next = unit.blocks.get(target)?;
            if seen.contains(target)
                || seen.len() >= 128
                || !next.instructions.iter().all(eligible)
                || instructions.len() + next.instructions.len() + args.len() * 2
                    > MAX_BLOCK_INSTRUCTIONS
            {
                break;
            }
            if next.params.len() != args.len() {
                return None;
            }
            // Snapshot all arguments before assigning any parameter, preserving
            // parallel phi transfers even when the source identities overlap.
            let mut copies = Vec::new();
            for (&arg, &param) in args.iter().zip(&next.params) {
                let temp = RegisterId(next_register);
                next_register = next_register.checked_add(1)?;
                extra_types.insert(temp, unit.register_map.get(&arg)?.clone());
                *consumed.entry(arg).or_default() += 1;
                instructions.push(SIRInstruction::Concat(temp, vec![arg]));
                copies.push(SIRInstruction::Concat(param, vec![temp]));
            }
            instructions.extend(copies);
            current = *target;
        }
        if instructions.len() < 16 {
            return None;
        }
        let mut inputs = BTreeSet::new();
        let mut outputs = BTreeSet::new();
        for inst in &instructions {
            inst.for_each_use(|reg| {
                if !outputs.contains(&reg) {
                    inputs.insert(reg);
                }
            });
            if let Some(dst) = inst.defined_register() {
                outputs.insert(dst);
            }
        }
        let types = inputs
            .union(&outputs)
            .map(|&reg| {
                let ty = extra_types
                    .get(&reg)
                    .or_else(|| unit.register_map.get(&reg))?;
                (ty.width() <= 4096).then(|| (reg, ty.clone()))
            })
            .collect::<Option<HashMap<_, _>>>()?;
        Some(Self {
            instructions,
            types,
            inputs,
            outputs,
            consumed,
            source: Arc::clone(source),
            tail: current,
        })
    }

    pub(super) fn compile(
        mut self,
        layout: &MemoryLayout,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
    ) -> Result<BlockCode, SimulatorError> {
        // Count original uses once on the worker. A definition is observable
        // after this region exactly when some use lies outside the consumed
        // bodies/jumps. In particular the final interpreted terminator counts
        // as outside, as do every cold edge and loop backedge.
        let global = self.source.uses();
        self.outputs.retain(|reg| {
            let total = global.get(reg).copied().unwrap_or(0);
            total == usize::MAX || total > self.consumed.get(reg).copied().unwrap_or(0)
        });
        let error = |message| SimulatorError::from(crate::CodegenError::message(message));
        let mut layout = layout.clone();
        let frame_start = layout
            .merged_total_size
            .checked_add(layout.triggered_bits_total_size)
            .and_then(|size| size.checked_add(7))
            .map(|size| size & !7)
            .ok_or_else(|| error("hotspot frame offset overflow"))?;
        let instance = layout
            .offsets
            .keys()
            .map(|addr| addr.instance_id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| error("hotspot private address overflow"))?;
        let mut cursor = frame_start;
        let mut slots = BTreeMap::new();
        for (index, &reg) in self.inputs.union(&self.outputs).enumerate() {
            let ty = &self.types[&reg];
            let bytes = ty.width().div_ceil(8);
            let four_state = options.four_state && matches!(ty, RegisterType::Logic { .. });
            let size = bytes
                .checked_mul(if four_state { 2 } else { 1 })
                .and_then(|size| size.checked_add(celox_state_layout::MAX_ACCESS_OVERREACH + 7))
                .map(|size| size & !7)
                .ok_or_else(|| error("hotspot frame size overflow"))?;
            let addr = AbsoluteAddr {
                instance_id: InstanceId(instance),
                var_id: celox_design::StateObjectId(
                    u32::try_from(index).map_err(|_| error("too many hotspot frame slots"))?,
                ),
            };
            layout.offsets.insert(addr, cursor);
            layout.widths.insert(addr, ty.width());
            layout.is_4states.insert(addr, four_state);
            slots.insert(
                reg,
                Slot {
                    addr: RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                    offset: cursor,
                    bytes,
                    width: ty.width(),
                    four_state,
                },
            );
            cursor = cursor
                .checked_add(size)
                .ok_or_else(|| error("hotspot frame size overflow"))?;
            if cursor - frame_start > MAX_FRAME_BYTES {
                return Err(error("hotspot frame exceeds budget"));
            }
        }
        // These addresses exist only in this compilation's view. They follow
        // every byte of the live image; existing semantic addresses and region
        // bases are unchanged. The emitter places scratch after this frame.
        layout.merged_total_size = cursor;
        let mut instructions = Vec::new();
        for &reg in &self.inputs {
            instructions.push(SIRInstruction::Load(
                reg,
                slots[&reg].addr,
                SIROffset::Static(0),
                self.types[&reg].width(),
            ));
        }
        instructions.extend(self.instructions);
        for &reg in &self.outputs {
            instructions.push(SIRInstruction::Store(
                slots[&reg].addr,
                SIROffset::Static(0),
                self.types[&reg].width(),
                reg,
                Vec::new(),
                Vec::new(),
            ));
        }
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: block.id,
            blocks: [(block.id, block)].into_iter().collect(),
            register_map: self.types,
        };
        let code = HotspotCode::compile(&[unit], &layout, options, cancel)?;
        let inputs: Vec<(RegisterId, Slot)> = self
            .inputs
            .into_iter()
            .map(|reg| (reg, slots[&reg].clone()))
            .collect();
        let outputs: Vec<(RegisterId, Slot)> = self
            .outputs
            .into_iter()
            .map(|reg| (reg, slots[&reg].clone()))
            .collect();
        let transferred_bytes = inputs
            .iter()
            .chain(&outputs)
            .map(|(_, slot): &(RegisterId, Slot)| slot.bytes * if slot.four_state { 2 } else { 1 })
            .sum();
        Ok(BlockCode {
            code,
            inputs,
            outputs,
            tail: self.tail,
            transferred_bytes,
        })
    }
}

#[derive(Clone)]
struct Slot {
    addr: RegionedAbsoluteAddr,
    offset: usize,
    bytes: usize,
    width: usize,
    four_state: bool,
}

pub(super) struct BlockCode {
    pub(super) code: HotspotCode,
    inputs: Vec<(RegisterId, Slot)>,
    outputs: Vec<(RegisterId, Slot)>,
    pub(super) tail: BlockId,
    pub(super) transferred_bytes: usize,
}

impl BlockCode {
    pub(super) fn run(
        &self,
        memory: &mut [u64],
        registers: &mut Registers,
    ) -> Result<(), InterpError> {
        if self.code.required_state_size > memory.len().saturating_mul(8) {
            return Err(InterpError::Machine(
                "hotspot frame exceeds live allocation".into(),
            ));
        }
        let write = |plane: &mut [u8], value: &BigUint| {
            plane.fill(0);
            for (chunk, word) in plane.chunks_mut(8).zip(value.iter_u64_digits()) {
                chunk.copy_from_slice(&word.to_le_bytes()[..chunk.len()]);
            }
        };
        // The frame lies within this exclusively borrowed allocation and all
        // offsets were checked before publication. Release the byte view before
        // native code uses the same allocation.
        {
            let bytes = unsafe {
                std::slice::from_raw_parts_mut(memory.as_mut_ptr().cast::<u8>(), memory.len() * 8)
            };
            for (reg, slot) in &self.inputs {
                let value = registers.get(*reg)?;
                write(
                    &mut bytes[slot.offset..slot.offset + slot.bytes],
                    &value.payload,
                );
                if slot.four_state {
                    write(
                        &mut bytes[slot.offset + slot.bytes..slot.offset + slot.bytes * 2],
                        &value.mask,
                    );
                }
            }
        }
        self.code.run(memory).map_err(|error| match error {
            SimulatorErrorCode::DetectedTrueLoopCode(code) => InterpError::Fatal(code),
            error => InterpError::Machine(format!("hotspot block failed: {error:?}")),
        })?;
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(memory.as_mut_ptr().cast::<u8>(), memory.len() * 8)
        };
        for (reg, slot) in &self.outputs {
            // Native partial stores preserve padding from the previous frame.
            // Strip it from both planes before exporting interpreter values.
            if !slot.width.is_multiple_of(8) {
                let mask = (1u8 << (slot.width % 8)) - 1;
                bytes[slot.offset + slot.bytes - 1] &= mask;
                if slot.four_state {
                    bytes[slot.offset + slot.bytes * 2 - 1] &= mask;
                }
            }
            let payload = BigUint::from_bytes_le(&bytes[slot.offset..slot.offset + slot.bytes]);
            let mask = if slot.four_state {
                BigUint::from_bytes_le(
                    &bytes[slot.offset + slot.bytes..slot.offset + slot.bytes * 2],
                )
            } else {
                BigUint::default()
            };
            registers.set(*reg, SIRValue { payload, mask });
        }
        Ok(())
    }
}
