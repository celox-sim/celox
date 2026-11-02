//! A bounded hot path with exits before the original interpreted terminator.
use super::*;

pub(in super::super) struct TraceRequest {
    source: Arc<RegionSource>,
    path: Vec<BlockId>,
}

impl TraceRequest {
    pub(super) fn new(
        source: &Arc<RegionSource>,
        start: BlockId,
        mut successor: impl FnMut(BlockId) -> Option<BlockId>,
    ) -> Option<Self> {
        let mut path = Vec::new();
        let mut count = 0;
        let mut current = start;
        let mut branches = 0;
        loop {
            let block = source.unit.blocks.get(&current)?;
            if path.contains(&current)
                || path.len() == 128
                || count + block.instructions.len() > MAX_BLOCK_INSTRUCTIONS
                || !block.instructions.iter().all(eligible)
            {
                break;
            }
            path.push(current);
            count += block.instructions.len();
            let next = match &block.terminator {
                SIRTerminator::Jump(target, _) => Some(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => successor(current)
                    .filter(|next| *next == true_block.0 || *next == false_block.0),
                SIRTerminator::Switch { cases, default, .. } => successor(current).filter(|next| {
                    *next == *default || cases.iter().any(|case| case.target == *next)
                }),
                _ => None,
            };
            let Some(next) = next else {
                break;
            };
            if !matches!(block.terminator, SIRTerminator::Jump(..)) {
                branches += 1;
            }
            current = next;
        }
        (path.len() > 1 && branches > 0 && count >= 16).then(|| Self {
            source: Arc::clone(source),
            path,
        })
    }

    pub(super) fn compile(
        mut self,
        layout: &MemoryLayout,
        options: &SimulatorOptions,
        cancel: &CompileCancel,
    ) -> Result<BlockCode, SimulatorError> {
        if cancel.is_cancelled() {
            return Err(crate::backend::compile_cancel::cancelled_error());
        }
        let error = |message| SimulatorError::from(crate::CodegenError::message(message));
        let source = &self.source;
        let unit = &source.unit;
        let start = self.path[0];
        let availability = source
            .analysis()
            .ok_or_else(|| error("hotspot input dominance unavailable"))?;
        let entry_index = availability.cfg.index[&start];
        // A trace entered inside a loop must exit before revisiting an
        // ancestor of its entry. Otherwise prologue imports and the ancestor
        // can define the same SSA register twice in the native fragment.
        if let Some(index) = self.path.iter().skip(1).position(|id| {
            availability
                .cfg
                .dominators
                .dominates(availability.cfg.index[id], entry_index)
        }) {
            self.path.truncate(index + 1);
        }

        let mut fresh_block = unit
            .blocks
            .keys()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| error("hotspot block identity overflow"))?;
        let prologue = BlockId(fresh_block);
        fresh_block = fresh_block
            .checked_add(1)
            .ok_or_else(|| error("hotspot block identity overflow"))?;
        let mut blocks = HashMap::default();
        let mut inputs = BTreeSet::new();
        let mut defined = BTreeSet::new();
        let mut consumed = HashMap::<RegisterId, usize>::default();
        let mut exit_specs = Vec::new();
        let global = source.uses();
        for (index, id) in self.path.iter().enumerate() {
            let mut block = unit.blocks[id].clone();
            if index == 0 {
                // Parameters already live in interpreter registers. Import only
                // those actually used by native instructions or hot edges.
                block.params.clear();
            } else {
                defined.extend(block.params.iter().copied());
            }
            for inst in &block.instructions {
                inst.for_each_use(|reg| {
                    if !defined.contains(&reg) {
                        inputs.insert(reg);
                    }
                    *consumed.entry(reg).or_default() += 1;
                });
                if let Some(reg) = inst.defined_register() {
                    defined.insert(reg);
                }
            }
            let exit = BlockId(fresh_block);
            let mut exits = false;
            let next = self.path.get(index + 1).copied();
            let mut edge = |target: &mut BlockId, args: &mut Vec<RegisterId>| {
                if Some(*target) != next {
                    *target = exit;
                    args.clear();
                    exits = true;
                }
            };
            if next.is_none() {
                block.terminator = SIRTerminator::Jump(exit, vec![]);
                exits = true;
            } else {
                match &mut block.terminator {
                    SIRTerminator::Jump(target, args) => edge(target, args),
                    SIRTerminator::Branch {
                        true_block,
                        false_block,
                        ..
                    } => {
                        edge(&mut true_block.0, &mut true_block.1);
                        edge(&mut false_block.0, &mut false_block.1);
                    }
                    SIRTerminator::Switch { cases, default, .. } => {
                        for case in cases {
                            edge(&mut case.target, &mut vec![]);
                        }
                        edge(default, &mut vec![]);
                    }
                    _ => unreachable!("trace has a successor only for control-flow edges"),
                }
            }
            // Count only original uses. At this exit the original terminator
            // has not executed, so its condition and arguments remain live.
            if exits {
                let outputs: Vec<_> = defined
                    .iter()
                    .copied()
                    .filter(|reg| {
                        let total = global.get(reg).copied().unwrap_or(0);
                        total == usize::MAX || total > consumed.get(reg).copied().unwrap_or(0)
                    })
                    .collect();
                exit_specs.push((exit, *id, outputs));
                fresh_block = fresh_block
                    .checked_add(1)
                    .ok_or_else(|| error("hotspot block identity overflow"))?;
            }
            block.terminator.for_each_use(|reg| {
                if !defined.contains(&reg) {
                    inputs.insert(reg);
                }
            });
            unit.blocks[id]
                .terminator
                .for_each_use(|reg| *consumed.entry(reg).or_default() += 1);
            blocks.insert(*id, block);
        }
        // Exit stores can otherwise grow quadratically with the path length.
        // Keep the existing chain as the bounded fallback for such traces.
        let expanded = blocks
            .values()
            .map(|block| block.instructions.len())
            .sum::<usize>()
            + inputs.len()
            + exit_specs
                .iter()
                .map(|(_, _, outputs)| outputs.len() + 2)
                .sum::<usize>();
        if expanded > MAX_BLOCK_INSTRUCTIONS * 4 {
            return ChainRequest::new(source, start)
                .ok_or_else(|| error("hotspot exit copies exceed instruction budget"))?
                .compile(layout, options, cancel);
        }
        // MIR phis identify incoming values by predecessor, not by branch
        // arm. Split parallel edges with arguments before native selection.
        let mut bridges = Vec::new();
        for block in blocks.values_mut() {
            if let SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } = &mut block.terminator
            {
                if true_block.0 == false_block.0 && !true_block.1.is_empty() {
                    for edge in [true_block, false_block] {
                        let id = BlockId(fresh_block);
                        fresh_block = fresh_block
                            .checked_add(1)
                            .ok_or_else(|| error("hotspot block identity overflow"))?;
                        bridges.push(BasicBlock {
                            id,
                            params: vec![],
                            instructions: vec![],
                            terminator: SIRTerminator::Jump(edge.0, std::mem::take(&mut edge.1)),
                        });
                        edge.0 = id;
                    }
                }
            }
        }
        blocks.extend(bridges.into_iter().map(|block| (block.id, block)));
        // Loads in the prologue must be valid even when the first guard exits.
        // Valid SSA inputs dominate the entry; reject ambiguous definitions.
        for input in &inputs {
            let available = match availability.definitions.get(input) {
                Some(Some(def)) if *def == start => unit.blocks[&start].params.contains(input),
                Some(Some(def)) => availability
                    .cfg
                    .dominators
                    .dominates(availability.cfg.index[def], availability.cfg.index[&start]),
                _ => false,
            };
            if defined.contains(input) || !available {
                return Err(error("hotspot input is unavailable at entry"));
            }
        }
        let mut types = HashMap::default();
        for reg in inputs.iter().chain(&defined) {
            let ty = unit
                .register_map
                .get(reg)
                .ok_or_else(|| error("missing hotspot register type"))?;
            if ty.width() > 4096 {
                return Err(error("hotspot register exceeds width budget"));
            }
            types.insert(*reg, ty.clone());
        }
        let selector = RegisterId(source.next_register);
        types.insert(
            selector,
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        let mut frame_regs = inputs.clone();
        for (_, _, outputs) in &exit_specs {
            frame_regs.extend(outputs);
        }
        frame_regs.insert(selector);
        let (layout, slots) = frame_layout(frame_regs, &types, layout, options.four_state)?;
        let mut loads = Vec::new();
        for &reg in &inputs {
            loads.push(SIRInstruction::Load(
                reg,
                slots[&reg].addr,
                SIROffset::Static(0),
                types[&reg].width(),
            ));
        }
        blocks.insert(
            prologue,
            BasicBlock {
                id: prologue,
                params: vec![],
                instructions: loads,
                terminator: SIRTerminator::Jump(start, vec![]),
            },
        );
        let input_bytes: usize = inputs.iter().map(|reg| slots[reg].transfer_bytes()).sum();
        let mut exits = Vec::new();
        // Each selector definition is unique in SSA, while sharing one private
        // frame address. Native return codes remain reserved for real errors.
        let mut next_reg = source.next_register;
        for (index, (id, tail, outputs)) in exit_specs.into_iter().enumerate() {
            next_reg = next_reg
                .checked_add(1)
                .ok_or_else(|| error("hotspot register identity overflow"))?;
            let value = RegisterId(next_reg);
            types.insert(
                value,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
            let mut instructions = Vec::new();
            for &reg in &outputs {
                instructions.push(SIRInstruction::Store(
                    slots[&reg].addr,
                    SIROffset::Static(0),
                    types[&reg].width(),
                    reg,
                    vec![],
                    vec![],
                ));
            }
            instructions.push(SIRInstruction::Imm(value, SIRValue::new(index as u64)));
            instructions.push(SIRInstruction::Store(
                slots[&selector].addr,
                SIROffset::Static(0),
                64,
                value,
                vec![],
                vec![],
            ));
            blocks.insert(
                id,
                BasicBlock {
                    id,
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            );
            exits.push(TraceExit {
                tail,
                transferred_bytes: input_bytes
                    + outputs
                        .iter()
                        .map(|reg| slots[reg].transfer_bytes())
                        .sum::<usize>(),
                outputs: outputs
                    .into_iter()
                    .map(|reg| (reg, slots[&reg].clone()))
                    .collect(),
            });
        }
        let compiled = ExecutionUnit {
            entry_block_id: prologue,
            blocks,
            register_map: types,
        };
        let code = HotspotCode::compile(&[compiled], &layout, options, cancel)?;
        Ok(BlockCode {
            code,
            inputs: inputs
                .into_iter()
                .map(|reg| (reg, slots[&reg].clone()))
                .collect(),
            outputs: vec![],
            tail: start,
            transferred_bytes: input_bytes,
            exits: Some((slots[&selector].clone(), exits)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SimBackend;

    #[test]
    fn hotspot_trace_exits_before_reentering_an_ancestor_definition() {
        let sim = crate::SimulatorBuilder::<crate::Simulator>::new(
            "module Top (q: output logic<130>) { assign q = 130'd0; }",
            "Top",
        )
        .four_state(true)
        .build_interpreter()
        .unwrap();
        let layout = sim.backend.layout();
        let addr = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            *layout.offsets.keys().next().unwrap(),
        );
        let mut b = celox_sir::SIRBuilder::new();
        let invariant = b.alloc_logic(130);
        let cond = b.alloc_bit(1, false);
        let body = b.new_block();
        let latch = b.new_block();
        let cold = b.new_block();
        b.emit(SIRInstruction::Imm(
            invariant,
            SIRValue::new_four_state(42u8, 3u8),
        ));
        b.emit(SIRInstruction::Imm(cond, SIRValue::new(1u8)));
        b.seal_block(SIRTerminator::Jump(body, vec![]));
        b.switch_to_block(body);
        for _ in 0..16 {
            let dead = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(dead, vec![invariant]));
        }
        b.emit(SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            130,
            invariant,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (latch, vec![]),
            false_block: (cold, vec![]),
        });
        b.switch_to_block(latch);
        b.seal_block(SIRTerminator::Jump(BlockId(0), vec![]));
        b.switch_to_block(cold);
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        let unit = Arc::new(ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        });
        let source = Arc::new(RegionSource::new(Arc::clone(&unit)));
        let code = TraceRequest::new(&source, body, |_| Some(latch))
            .unwrap()
            .compile(
                layout,
                &SimulatorOptions {
                    four_state: true,
                    ..Default::default()
                },
                &CompileCancel::new(),
            )
            .unwrap();
        let mut memory = vec![0; code.code.required_state_size.div_ceil(8)];
        let mut regs = Registers::new(&unit.register_map, true);
        let imported = SIRValue::new_four_state(37u8, BigUint::from(1u8) << 128usize);
        regs.set(invariant, imported.clone());
        regs.set(cond, SIRValue::new(1u8));
        assert_eq!(code.run(&mut memory, &mut regs).unwrap().0, latch);
        assert_eq!(regs.get(invariant).unwrap(), &imported);
        assert_eq!(memory[layout.region_base_offset(&addr) / 8], 37);
    }

    // Every exit has a distinct set of initialized wide registers. Reuse the
    // same native frame after taking a later exit to catch stale exports.
    #[test]
    fn hotspot_trace_exports_only_executed_prefix_and_preserves_wide_phi_values() {
        for (switch, four_state) in [(false, false), (false, true), (true, false), (true, true)] {
            let sim = crate::SimulatorBuilder::<crate::Simulator>::new(
                "module Top (q: output logic<130>) { assign q = 130'd0; }",
                "Top",
            )
            .four_state(four_state)
            .build_interpreter()
            .unwrap();
            let layout = sim.backend.layout();
            let mut b = celox_sir::SIRBuilder::<RegionedAbsoluteAddr>::new();
            let cond = b.alloc_logic(if switch { 2 } else { 1 });
            let left = b.alloc_logic(130);
            let right = b.alloc_logic(130);
            let unused = b.alloc_logic(130);
            let p = b.alloc_logic(130);
            let q = b.alloc_logic(130);
            let hot = b.new_block_with(if switch { vec![] } else { vec![p, q] });
            let cold = b.new_block();
            let join = b.new_block();
            for _ in 0..16 {
                let dead = b.alloc_logic(130);
                b.emit(SIRInstruction::Concat(dead, vec![left]));
            }
            let prefix = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(prefix, vec![left]));
            b.seal_block(if switch {
                SIRTerminator::Switch {
                    selector: cond,
                    cases: vec![celox_sir::SIRSwitchCase {
                        value: 1u8.into(),
                        target: hot,
                    }],
                    default: cold,
                }
            } else {
                SIRTerminator::Branch {
                    cond,
                    true_block: (hot, vec![right, left]),
                    false_block: (cold, vec![]),
                }
            });
            b.switch_to_block(hot);
            let out = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(
                out,
                vec![if switch { right } else { p }],
            ));
            b.seal_block(SIRTerminator::Jump(join, vec![]));
            b.switch_to_block(join);
            let joined = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(joined, vec![out]));
            b.seal_block(SIRTerminator::Jump(
                BlockId(0),
                vec![cond, joined, prefix, left],
            ));
            b.switch_to_block(cold);
            // A cold-only definition must not become an eager frame input.
            let cold_only = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(cold_only, vec![prefix]));
            b.seal_block(SIRTerminator::Error(42));
            let (mut blocks, register_map, _) = b.drain();
            blocks.get_mut(&BlockId(0)).unwrap().params = vec![cond, left, right, unused];
            let unit = Arc::new(ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks,
                register_map,
            });
            let source = Arc::new(RegionSource::new(Arc::clone(&unit)));
            let request = TraceRequest::new(&source, BlockId(0), |_| Some(hot)).unwrap();
            assert_eq!(request.path, [BlockId(0), hot, join]);
            let code = request
                .compile(
                    layout,
                    &SimulatorOptions {
                        four_state,
                        ..Default::default()
                    },
                    &CompileCancel::new(),
                )
                .unwrap();
            assert!(!code.inputs.iter().any(|(reg, _)| *reg == unused));
            let mut a = SIRValue::new_four_state(BigUint::from(1u8) << 129usize, 3u8);
            let mut z = SIRValue::new_four_state(5u8, BigUint::from(1u8) << 128usize);
            if !four_state {
                a.mask = BigUint::default();
                z.mask = BigUint::default();
            }
            let mut memory = vec![0; code.code.required_state_size.div_ceil(8)];
            let mut hot_bytes = 0;
            for (iteration, take_hot) in [true, false, true, false].into_iter().enumerate() {
                let mut regs = Registers::new(&unit.register_map, four_state);
                regs.set(
                    cond,
                    if iteration == 3 && four_state && !switch {
                        SIRValue::new_four_state(1u8, 1u8)
                    } else {
                        SIRValue::new(u8::from(take_hot))
                    },
                );
                regs.set(left, a.clone());
                regs.set(right, z.clone());
                let (tail, bytes) = code.run(&mut memory, &mut regs).unwrap();
                assert_eq!(regs.get(prefix).unwrap(), &a);
                assert!(regs.get(cold_only).is_err());
                if take_hot {
                    assert_eq!(tail, join);
                    assert_eq!(regs.get(joined).unwrap(), &z);
                    hot_bytes = bytes;
                } else {
                    assert_eq!(tail, BlockId(0));
                    assert!(regs.get(joined).is_err());
                    assert!(regs.get(out).is_err());
                    assert!(bytes < hot_bytes);
                }
            }
        }
    }

    #[test]
    fn hotspot_trace_keeps_distinct_arguments_for_edges_to_the_same_target() {
        let sim = crate::SimulatorBuilder::<crate::Simulator>::new(
            "module Top (q: output logic<130>) { assign q = 130'd0; }",
            "Top",
        )
        .four_state(true)
        .build_interpreter()
        .unwrap();
        let mut b = celox_sir::SIRBuilder::<RegionedAbsoluteAddr>::new();
        let cond = b.alloc_bit(1, false);
        let a = b.alloc_logic(130);
        let z = b.alloc_logic(130);
        let p = b.alloc_logic(130);
        let q = b.alloc_logic(130);
        let hot = b.new_block_with(vec![p, q]);
        for _ in 0..16 {
            let dead = b.alloc_logic(130);
            b.emit(SIRInstruction::Concat(dead, vec![a]));
        }
        b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (hot, vec![a, z]),
            false_block: (hot, vec![z, a]),
        });
        b.switch_to_block(hot);
        b.seal_block(SIRTerminator::Jump(BlockId(0), vec![cond, p, q]));
        let (mut blocks, register_map, _) = b.drain();
        blocks.get_mut(&BlockId(0)).unwrap().params = vec![cond, a, z];
        let unit = Arc::new(ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        });
        let source = Arc::new(RegionSource::new(Arc::clone(&unit)));
        let code = TraceRequest::new(&source, BlockId(0), |_| Some(hot))
            .unwrap()
            .compile(
                sim.backend.layout(),
                &SimulatorOptions {
                    four_state: true,
                    ..Default::default()
                },
                &CompileCancel::new(),
            )
            .unwrap();
        let a_value = SIRValue::new_four_state(BigUint::from(1u8) << 129usize, 7u8);
        let z_value = SIRValue::new_four_state(3u8, BigUint::from(1u8) << 128usize);
        let mut memory = vec![0; code.code.required_state_size.div_ceil(8)];
        for select in [false, true] {
            let mut regs = Registers::new(&unit.register_map, true);
            regs.set(cond, SIRValue::new(u8::from(select)));
            regs.set(a, a_value.clone());
            regs.set(z, z_value.clone());
            assert_eq!(code.run(&mut memory, &mut regs).unwrap().0, hot);
            assert_eq!(
                regs.get(p).unwrap(),
                if select { &a_value } else { &z_value }
            );
            assert_eq!(
                regs.get(q).unwrap(),
                if select { &z_value } else { &a_value }
            );
        }
    }
}
