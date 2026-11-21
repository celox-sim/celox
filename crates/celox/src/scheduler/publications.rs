//! Publish the changed register inputs of a clocked unit in the blocks that
//! write them. Each block merges its notifications into the pending mask;
//! the shared settle consumes that mask after simultaneous writers finish.
use super::*;
use celox_sir::RegisterType;

impl Generator<'_> {
    pub(super) fn publication(&self, address: AbsoluteAddr) -> Option<(usize, u64)> {
        let home = self.shared.get(&address).copied().unwrap_or(address);
        let schedule = self.incremental?.events.as_ref()?;
        let readers = *schedule.publications.get(&home)?;
        Some((schedule.publication_offset, readers))
    }

    pub(super) fn clock_publications(&self, unit: &Unit) -> Option<Unit> {
        // Raw wide commits have typed shadows for planning only. Keep their
        // existing clock notification, as well as calls with object effects.
        let relevant = |address: &RegionedAbsoluteAddr| {
            address.region == STABLE_REGION && self.settle_reads(address.absolute_addr())
        };
        if instructions(unit).any(|inst| {
            crate::settle::is_wide_commit_access(inst)
                || written_storage(inst)
                    .filter(|addr| relevant(addr))
                    .any(|addr| {
                        inst.memory_write().is_none()
                            || self.publication(addr.absolute_addr()).is_none()
                    })
        }) {
            return None;
        }
        let offset = self.incremental?.events.as_ref()?.publication_offset;
        let mut body = unit.clone();
        let mut next = unit.register_map.keys().map(|r| r.0 + 1).max().unwrap_or(0);
        let mut register = |width, logic| {
            let ty = if logic {
                RegisterType::Logic { width }
            } else {
                RegisterType::Bit {
                    width,
                    signed: false,
                }
            };
            let id = RegisterId(next);
            next += 1;
            body.register_map.insert(id, ty);
            id
        };
        for block in body.blocks.values_mut() {
            let old = std::mem::take(&mut block.instructions);
            let out = &mut block.instructions;
            let mut changes = BTreeMap::new();
            for inst in old {
                let Some(write) = inst.memory_write().filter(|w| relevant(w.addr)) else {
                    out.push(inst);
                    continue;
                };
                let address = *write.addr;
                let (_, readers) = self.publication(address.absolute_addr())?;
                // A dynamic or wide write marks its readers only if this
                // block executes, without copying a memory to compare it.
                let sparse = match &inst {
                    SIRInstruction::Commit(source, ..)
                        if self.outlined.is_some()
                            && source.region == celox_design::SPARSE_WORKING_REGION =>
                    {
                        self.program
                            .layout()
                            .sparse_layouts
                            .get(&source.absolute_addr())
                            .filter(|s| (1..=8).contains(&s.summary_word_count))
                    }
                    _ => None,
                };
                let layout = self.program.layout();
                let comparable = match write.offset {
                    SIROffset::Static(offset) => {
                        offset
                            .checked_add(write.width)
                            .is_some_and(|end| end <= layout.widths[&address.absolute_addr()])
                            && !crate::settle::spans_padded_elements(
                                layout,
                                &address,
                                write.offset,
                                write.width,
                            )
                    }
                    SIROffset::Element {
                        element_width,
                        bit_offset,
                        dynamic_bit_offset: None,
                        ..
                    } => bit_offset
                        .checked_add(write.width)
                        .is_some_and(|end| end <= *element_width),
                    _ => false,
                };
                let changed = match sparse {
                    _ if comparable
                        && (1..=256).contains(&write.width)
                        && (write.width <= 64 || sparse.is_none()) =>
                    {
                        let width = write.width;
                        let logic = self.keeps_unknowns(address.absolute_addr());
                        let before = register(width, logic);
                        let offset = write.offset.clone();
                        out.push(SIRInstruction::Load(before, address, offset.clone(), width));
                        // A value already in the destination's representation
                        // can be compared directly to the old register.
                        let value = match &inst {
                            SIRInstruction::Store(_, _, _, src, _, _)
                                if unit.register_map[src].width() == width
                                    && (logic
                                        || matches!(
                                            unit.register_map[src],
                                            RegisterType::Bit { .. }
                                        )) =>
                            {
                                Some(*src)
                            }
                            _ => None,
                        };
                        out.push(inst);
                        let after = value.unwrap_or_else(|| {
                            let value = register(width, logic);
                            out.push(SIRInstruction::Load(value, address, offset, width));
                            value
                        });
                        let changed = register(1, false);
                        out.push(SIRInstruction::Binary(
                            changed,
                            before,
                            BinaryOp::NeCase,
                            after,
                        ));
                        changed
                    }
                    Some(sparse) => {
                        let written =
                            crate::settle::sparse_written(out, sparse, |w| register(w, false));
                        out.push(inst);
                        written
                    }
                    _ => {
                        out.push(inst);
                        let one = register(1, false);
                        out.push(SIRInstruction::Imm(one, SIRValue::new(1u64)));
                        one
                    }
                };
                if let Some(previous) = changes.insert(readers, changed) {
                    let either = register(1, false);
                    out.push(SIRInstruction::Binary(
                        either,
                        previous,
                        BinaryOp::Or,
                        changed,
                    ));
                    changes.insert(readers, either);
                }
            }
            if !changes.is_empty() {
                let flags = stable(self.slots.settle_events);
                let offset = SIROffset::Static(offset);
                let mut pending = register(64, false);
                let zero = register(64, false);
                out.push(SIRInstruction::Load(pending, flags, offset.clone(), 64));
                out.push(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                for (readers, changed) in changes {
                    let mask = register(64, false);
                    let select = register(64, false);
                    let combined = register(64, false);
                    out.push(SIRInstruction::Imm(mask, SIRValue::new(readers)));
                    out.push(SIRInstruction::Mux(select, changed, mask, zero));
                    out.push(SIRInstruction::Binary(
                        combined,
                        pending,
                        BinaryOp::Or,
                        select,
                    ));
                    pending = combined;
                }
                out.push(SIRInstruction::Store(
                    flags,
                    offset,
                    64,
                    pending,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }
        Some(body)
    }
}
