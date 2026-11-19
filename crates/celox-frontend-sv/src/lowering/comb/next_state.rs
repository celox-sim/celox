//! The next-state form of an `always_ff`: for each bit range a nonblocking
//! assignment of the process writes, the value the range takes at the
//! process's edge, as an expression of the values before it; and for each
//! nonblocking write of a memory element at a run-time index, a write port.
//!
//! The body executes symbolically as a combinational process does, except
//! that a nonblocking assignment writes a hidden next-state variable of its
//! target instead of the target: the process keeps reading the values
//! before the edge (IEEE 1800-2023 10.4.2). A range a path does not assign
//! keeps its value, which the hidden variable reads as the target's.
//!
//! A memory element written at a run-time index would make the whole
//! memory a next-state value. Such a write is a [`RamWritePort`] instead:
//! the element, the value and the condition the write happens under, all
//! evaluated at the statement. The ports apply after the roots, in source
//! order, so a later port overrides an earlier root of the same bits; a
//! constant-index write of a memory after one of its ports is not taken.
//!
//! A process this form does not take keeps its imperative units alone.

use super::assign::{Offset, Target};
use super::*;

/// One bit range of a target and its next value.
pub(in crate::lowering) type NextStateRoot = celox_slt::next_state::FlopRoot<SourceVarId>;

/// A nonblocking write of an unpacked-array element at a run-time index.
pub(in crate::lowering) type RamWritePort = celox_slt::next_state::RamWritePort<SourceVarId>;

/// The next-state form of one process.
pub(in crate::lowering) type NextState = celox_slt::next_state::NextStateForm<SourceVarId>;

/// Where the nonblocking assignments of the next-state form go while the
/// body executes.
#[derive(Default)]
pub(super) struct Form {
    /// The hidden next-state variable of each target.
    shadows: HashMap<SourceVarId, SourceVarId>,
    ports: Vec<RamWritePort>,
}

impl Comb<'_, '_> {
    fn form(&mut self) -> &mut Form {
        self.next_state.as_mut().expect("the next-state form")
    }

    /// The target a nonblocking assignment of the next-state form writes:
    /// the hidden next-state variable of `target`'s variable.
    pub(super) fn next_target(&mut self, target: Target) -> Result<Target, sv::AnalyzerError> {
        let id = match &target {
            Target::Static { id, .. } | Target::Dynamic { id, .. } => *id,
        };
        if self.form().ports.iter().any(|port| port.array == id) {
            return Err(unsupported(
                "a constant-index write of a memory after its write port in the next-state form",
            ));
        }
        let shadow = match self.form().shadows.get(&id) {
            Some(&shadow) => shadow,
            None => {
                let var = self.m.var(id);
                let (width, signed, is_4state) = (var.width, var.signed, var.is_4state);
                let (shadow, _) = self.m.module_temp("$next", width, signed, is_4state);
                self.form().shadows.insert(id, shadow);
                shadow
            }
        };
        Ok(match target {
            Target::Static {
                access,
                width,
                drop,
                lvalue,
                ..
            } => Target::Static {
                id: shadow,
                access,
                width,
                drop,
                lvalue,
            },
            Target::Dynamic { offset, width, .. } => Target::Dynamic {
                id: shadow,
                offset,
                width,
            },
        })
    }

    /// Record the write port of a nonblocking write of `value` to a memory
    /// element at a run-time index.
    pub(super) fn write_port(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        target: Target,
        value: NodeId,
    ) -> Result<(), sv::AnalyzerError> {
        let Target::Dynamic {
            id,
            offset:
                Offset::Element {
                    offset,
                    element_width,
                    element_count,
                    access,
                },
            ..
        } = target
        else {
            unreachable!("a write port writes a memory element at a run-time index");
        };
        // A port records one write; a loop fold repeats its body.
        if self.folds.folding > 0 || self.folds.probing > 0 {
            return Err(unsupported("a memory write port in a loop fold"));
        }
        let offset_expr =
            expr_from_select_position(&offset, self.m.constants, self.m.parameter_types)
                .ok_or_else(|| unsupported("dynamic assignment index"))?;
        let (bit_offset, _) = self.eval(store, frames, &offset_expr, None)?;
        let bit_offset =
            coerce_node_width(self.arena, bit_offset, Some(64), false).map_err(slt_error)?;
        let stride = self.constant(element_width as u64, 64)?;
        let index = self.alloc(SLTNode::Binary(bit_offset, BinaryOp::DivU, stride))?;
        // An unknown or out-of-range index writes nothing (IEEE 1800-2023
        // 7.4.6).
        let (_, valid) = dynamic_array_index_guard_slt(self.arena, index, element_count)
            .ok_or_else(|| unsupported("dynamic assignment index"))?;
        let guard = match self.path_condition()? {
            Some((path, _)) => slt_and(self.arena, &mut self.consts, path, valid)?,
            None => valid,
        };
        self.form().ports.push(RamWritePort {
            array: id,
            index,
            element_width,
            access,
            value,
            guard,
        });
        Ok(())
    }

    /// The next-state form of an `always_ff` body, or why the form does
    /// not take it.
    pub(in crate::lowering) fn lower_next_state(
        &mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<NextState, sv::AnalyzerError> {
        self.allow_nonblocking = true;
        self.hold_latches = true;
        self.next_state = Some(Form::default());
        let store = self.exec_block(Store::default(), &[], body)?;
        if !self.events.sites.is_empty() || !self.events.observers.is_empty() {
            return Err(unsupported("runtime events in the next-state form"));
        }
        let Form { shadows, ports } = self.next_state.take().unwrap_or_default();
        let targets: HashMap<SourceVarId, SourceVarId> = shadows
            .iter()
            .map(|(&target, &shadow)| (shadow, target))
            .collect();
        // A blocking write to a variable others see is visible at once, in
        // the Active region: it is no next state.
        if store
            .keys()
            .any(|id| !targets.contains_key(id) && !self.m.is_hidden(*id))
        {
            return Err(unsupported(
                "blocking write to a module variable in the next-state form",
            ));
        }
        // A forced target keeps its forced value while the process writes
        // it (IEEE 1800-2023 10.6.2), which only its units model.
        let forced = |id: &SourceVarId| {
            self.m.forceable.contains_key(id)
                || self.m.procedurally_forced.contains(id)
                || self.m.force_holds.contains_key(id)
        };
        if shadows
            .keys()
            .chain(ports.iter().map(|port| &port.array))
            .any(forced)
        {
            return Err(unsupported(
                "a write of a forceable variable in the next-state form",
            ));
        }
        let mut memo = HashMap::default();
        let mut form = NextState::default();
        let mut shadow_ids: Vec<_> = targets.keys().copied().collect();
        shadow_ids.sort_by_key(|id| id.0);
        for shadow in shadow_ids {
            let target = targets[&shadow];
            let Some(range) = store.get(&shadow) else {
                continue;
            };
            let ranges: Vec<_> = range
                .ranges
                .iter()
                .filter_map(|(&lsb, (value, width, origin))| {
                    value
                        .as_ref()
                        .map(|(node, _)| (lsb, *node, *width, *origin))
                })
                .collect();
            for (lsb, node, width, origin) in ranges {
                // A range split off a wider value holds that value's bits
                // from where it was placed.
                let node =
                    self.slice(node, BitAccess::new(lsb - origin, lsb - origin + width - 1))?;
                let value = self.read_targets(node, &targets, &mut memo)?;
                form.roots.push(NextStateRoot {
                    target,
                    access: BitAccess::new(lsb, lsb + width - 1),
                    value,
                });
            }
        }
        for port in ports {
            form.ports.push(RamWritePort {
                index: self.read_targets(port.index, &targets, &mut memo)?,
                value: self.read_targets(port.value, &targets, &mut memo)?,
                guard: self.read_targets(port.guard, &targets, &mut memo)?,
                ..port
            });
        }
        Ok(form)
    }

    /// `node` with each read of a hidden next-state variable, the value a
    /// path keeps, made a read of its target.
    fn read_targets(
        &mut self,
        node: NodeId,
        targets: &HashMap<SourceVarId, SourceVarId>,
        memo: &mut HashMap<NodeId, NodeId>,
    ) -> Result<NodeId, sv::AnalyzerError> {
        if let Some(&done) = memo.get(&node) {
            return Ok(done);
        }
        let rebuilt = match self.arena.get(node).clone() {
            SLTNode::Input {
                variable,
                signed,
                index,
                access,
            } => {
                let mut entries = Vec::with_capacity(index.len());
                for mut entry in index {
                    entry.node = self.read_targets(entry.node, targets, memo)?;
                    entries.push(entry);
                }
                SLTNode::Input {
                    variable: targets.get(&variable).copied().unwrap_or(variable),
                    signed,
                    index: entries,
                    access,
                }
            }
            SLTNode::Binary(lhs, op, rhs) => SLTNode::Binary(
                self.read_targets(lhs, targets, memo)?,
                op,
                self.read_targets(rhs, targets, memo)?,
            ),
            SLTNode::Unary(op, operand) => {
                SLTNode::Unary(op, self.read_targets(operand, targets, memo)?)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => SLTNode::Mux {
                cond: self.read_targets(cond, targets, memo)?,
                then_expr: self.read_targets(then_expr, targets, memo)?,
                else_expr: self.read_targets(else_expr, targets, memo)?,
            },
            SLTNode::Slice { expr, access } => SLTNode::Slice {
                expr: self.read_targets(expr, targets, memo)?,
                access,
            },
            SLTNode::Concat(parts) => {
                let mut rebuilt = Vec::with_capacity(parts.len());
                for (part, width) in parts {
                    rebuilt.push((self.read_targets(part, targets, memo)?, width));
                }
                SLTNode::Concat(rebuilt)
            }
            SLTNode::Constant(..) => {
                memo.insert(node, node);
                return Ok(node);
            }
            _ => return Err(unsupported("a loop fold in the next-state form")),
        };
        let id = self.alloc(rebuilt)?;
        memo.insert(node, id);
        Ok(id)
    }
}
