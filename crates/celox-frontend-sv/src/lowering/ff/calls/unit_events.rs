//! Event controls in a function unit (IEEE 1800-2023 9.4.2): counted at
//! the module's wait sites, or sampled in the frame.

use super::*;

impl Ff<'_, '_> {
    /// An event control in the unit being lowered (IEEE 1800-2023 9.4.2).
    ///
    /// Each occurrence of an item is counted where it happens, as for a
    /// kernel, when the items are module variables whose `iff` is either a
    /// module variable or reads only the unit's frame. Such a frame-only
    /// condition, such as whether a virtual interface the frame holds
    /// denotes an instance (the analyzer makes one item per instance it may
    /// denote), cannot change while the process waits: the unit evaluates
    /// it at the start of the wait and waits at the site whose terms are
    /// the items it selects, one site for each selection, among the sites
    /// it lays out for the control. Items whose conditions compare one value
    /// with distinct constants select at most one of them; otherwise each
    /// set of them is a site, for up to [`MAX_SELECTED_ITEMS`] such items.
    ///
    /// Any other item reads the frame in its expression: the unit keeps a
    /// sample of each item in its frame, waits for a change of what the
    /// items read, and at each resume compares each item with its sample by
    /// the edge rules of the item (Table 9-2), waiting again when none
    /// occurred; samples in module variables, as a kernel's `$event[k]`
    /// proxies take them, would be shared by every process waiting at the
    /// site.
    pub(in crate::lowering::ff) fn unit_wait_event(
        &mut self,
        items: &[sv::ir::EventItem],
    ) -> Result<(), sv::AnalyzerError> {
        let items: Vec<sv::ir::EventItem> = items
            .iter()
            .map(|item| sv::ir::EventItem {
                edge: item.edge,
                expr: self.propagate(&item.expr),
                iff: item.iff.as_ref().map(|iff| self.propagate(iff)),
            })
            .collect();
        let shared = |this: &Self, expr: &sv::ir::Expr, one_bit: bool| match expr {
            sv::ir::Expr::Ident(name) => this.m.id(name).is_some_and(|id| {
                let variable = this.m.var(id);
                !this.m.is_hidden(id)
                    && variable.array_dims.is_empty()
                    && (!one_bit || (variable.width == 1 && variable.float.is_none()))
            }),
            _ => false,
        };
        // Items that read no slot of the frame are those of a kernel's event
        // control, over module storage, static storage of subroutines
        // included: the kernel's terms, gates and proxies serve them.
        if items.iter().all(|item| {
            !self.reads_frame(&item.expr)
                && item.iff.as_ref().is_none_or(|iff| !self.reads_frame(iff))
        }) {
            let mut terms = Vec::new();
            let mut gates = Vec::new();
            let mut samples = Vec::new();
            self.event_terms(&items, &mut terms, &mut gates, &mut samples)?;
            return self.suspend_on(terms, gates, samples);
        }
        // The items whose condition the frame decides, by position.
        let mut selected = Vec::new();
        let counted = items.iter().enumerate().all(|(position, item)| {
            shared(self, &item.expr, false)
                && match &item.iff {
                    None => true,
                    Some(iff) if self.frame_only(iff) => {
                        selected.push(position);
                        true
                    }
                    Some(iff) => !self.reads_frame(iff),
                }
        });
        if !counted {
            return self.unit_sampled_event(&items);
        }
        // The weight of each selected item in the number of the site: with
        // conditions that select at most one item, its rank; otherwise a bit.
        let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
        let value = |expr: &sv::ir::Expr| {
            let constant = match expr {
                sv::ir::Expr::Literal(literal) => sv::ir::ConstExpr::Literal(literal.clone()),
                sv::ir::Expr::Ident(name) if constants.contains_key(name) => {
                    sv::ir::ConstExpr::Ident(name.clone())
                }
                _ => return None,
            };
            sv::typecheck::eval_const_expr_with_types(&constant, constants, parameter_types)
        };
        let exclusive = exclusive_selection(
            selected.iter().map(|&position| {
                items[position]
                    .iff
                    .as_ref()
                    .expect("a selected item has a condition")
            }),
            value,
        );
        if !exclusive && selected.len() > MAX_SELECTED_ITEMS {
            return self.unit_sampled_event(&items);
        }
        let selections: Vec<Vec<usize>> = if exclusive {
            std::iter::once(Vec::new())
                .chain(selected.iter().map(|&position| vec![position]))
                .collect()
        } else {
            (0..1usize << selected.len())
                .map(|mask| {
                    (0..selected.len())
                        .filter(|bit| mask & (1 << bit) != 0)
                        .map(|bit| selected[bit])
                        .collect()
                })
                .collect()
        };
        let mut first = None;
        let mut samples = Vec::new();
        for chosen in &selections {
            let site_items: Vec<sv::ir::EventItem> = items
                .iter()
                .enumerate()
                .filter(|(position, _)| !selected.contains(position) || chosen.contains(position))
                .map(|(position, item)| sv::ir::EventItem {
                    edge: item.edge,
                    expr: item.expr.clone(),
                    iff: if selected.contains(&position) {
                        None
                    } else {
                        item.iff.clone()
                    },
                })
                .collect();
            let mut terms = Vec::new();
            let mut gates = Vec::new();
            self.event_terms(&site_items, &mut terms, &mut gates, &mut samples)?;
            if gates.iter().all(Option::is_none) {
                gates.clear();
            }
            let site = self.wait_site(terms, gates);
            first.get_or_insert(site);
        }
        self.unit_samples(std::mem::take(&mut samples))?;
        let first = first.expect("one site at least");
        // The number of the site the frame selects now.
        let mut index = sv::ir::Expr::Literal("64'd0".into());
        for (rank, &position) in selected.iter().enumerate() {
            let weight = if exclusive { rank + 1 } else { 1 << rank };
            index = binary(
                index,
                sv::ir::BinaryOp::Add,
                sv::ir::Expr::Mux {
                    condition: Box::new(iff_truth(
                        items[position]
                            .iff
                            .clone()
                            .expect("a selected item has a condition"),
                    )),
                    then_expr: Box::new(sv::ir::Expr::Literal(format!("64'd{weight}"))),
                    else_expr: Box::new(sv::ir::Expr::Literal("64'd0".into())),
                },
            );
        }
        let index = self.integer_of_width(&index, 64, false)?;
        let base = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(base, SIRValue::new(u64::from(first))));
        let word = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(word, base, BinaryOp::Add, index));
        self.unit_wait_word(word)?;
        self.unit_flush_point();
        Ok(())
    }

    /// Whether variable `name` is a slot of the frame of the unit being
    /// lowered: an automatic of the unit or of a body it expands, or a
    /// temporary of its lowering. A static formal or local of a static
    /// subroutine is module-wide storage, which another call may change.
    pub(super) fn frame_slot(&self, name: &str) -> bool {
        self.m
            .id(name)
            .is_some_and(|id| self.m.is_hidden(id) && self.automatic_of_unit(id, name))
    }

    /// Whether `expr` reads only the frame of the unit being lowered and
    /// constants, and calls nothing: its value does not change while the
    /// process waits, since only the process writes its frame.
    pub(in crate::lowering::ff) fn frame_only(&self, expr: &sv::ir::Expr) -> bool {
        let mut calls = Vec::new();
        collect_calls(expr, &mut calls);
        let mut names = HashSet::default();
        super::super::super::procedural::expr_idents(expr, &mut names);
        calls.is_empty()
            && names
                .iter()
                .all(|name| self.m.constants.contains_key(name) || self.frame_slot(name))
    }

    /// Whether `expr` reads a slot of the frame of the unit being lowered.
    pub(super) fn reads_frame(&self, expr: &sv::ir::Expr) -> bool {
        let mut names = HashSet::default();
        super::super::super::procedural::expr_idents(expr, &mut names);
        names.iter().any(|name| self.frame_slot(name))
    }

    /// Whether `expr` reads a hidden variable that is not a slot of the
    /// frame: static storage of a subroutine, which a concurrent call may
    /// change.
    pub(super) fn reads_static_storage(&self, expr: &sv::ir::Expr) -> bool {
        let mut names = HashSet::default();
        super::super::super::procedural::expr_idents(expr, &mut names);
        names.iter().any(|name| {
            self.m
                .id(name)
                .is_some_and(|id| self.m.is_hidden(id) && !self.frame_slot(name))
        })
    }

    /// Take `samples` of an event control of the unit being lowered, as a
    /// kernel takes them before it suspends (see `Ff::event_expression`):
    /// the proxies and their operand copies are module variables, which
    /// every process waiting there may share, since the operands are the
    /// module's storage, never the frame's.
    pub(in crate::lowering::ff) fn unit_samples(
        &mut self,
        samples: Vec<(String, sv::ir::Expr)>,
    ) -> Result<(), sv::AnalyzerError> {
        for (target, value) in samples {
            if let Some(id) = self.m.id(&target)
                && let Some(resume) = self.resume_frame()
            {
                resume.staging.insert(id);
            }
            self.assign(&sv::ir::LValue::Ident(target), &value)?;
        }
        Ok(())
    }

    /// The event control `items` by samples in the frame (see
    /// [`Self::unit_wait_event`]).
    pub(super) fn unit_sampled_event(
        &mut self,
        items: &[sv::ir::EventItem],
    ) -> Result<(), sv::AnalyzerError> {
        let mut ids = Vec::new();
        let mut samples = Vec::new();
        for item in items {
            let expr = item.expr.clone();
            // The unit evaluates the items, and their gates, at each resume.
            self.check_event_purity(&expr)?;
            if let Some(iff) = &item.iff {
                self.check_event_purity(iff)?;
            }
            if !matches!(item.edge, sv::ir::EventEdge::Any)
                && self.m.expr_float_precision(&expr).is_some()
            {
                return Err(sv::AnalyzerError::RealEdgeEvent {
                    edge: match item.edge {
                        sv::ir::EventEdge::Pos => "posedge",
                        sv::ir::EventEdge::Neg => "negedge",
                        _ => "edge",
                    },
                });
            }
            ids.extend(self.level_terms(&expr)?);
            // An occurrence counts only if the `iff` condition holds when it
            // happens (IEEE 1800-2023 9.4.2.3), which this comparison at the
            // resume cannot judge for a condition that may change meanwhile;
            // a kernel's activation probe and gate counters do. A condition
            // on the frame alone cannot change during the wait: its value at
            // the start is its value at every occurrence.
            // An operand in the static storage of a subroutine, which a
            // concurrent call may change, neither wakes the unit nor is in
            // its frame: the unit stays inlined, where a kernel watches it.
            if self.reads_static_storage(&expr) {
                return Err(unsupported(
                    "an event control on the frame of a unit reading static storage",
                ));
            }
            if item.iff.as_ref().is_some_and(|iff| !self.frame_only(iff)) {
                return Err(unsupported(
                    "an event control on the frame of a unit gated by module state",
                ));
            }
            let float = self.m.expr_float_precision(&expr);
            let width = match float {
                Some(celox_sir::FloatPrecision::F64) => 64,
                Some(celox_sir::FloatPrecision::F32) => 32,
                None => sv_expr_natural_width(
                    &self.m.typed_calls(&expr),
                    self.m.variables,
                    self.m.name_to_id,
                    self.m.constants,
                    self.m.parameter_types,
                )
                .ok_or_else(|| unsupported("event expression of unknown width"))?,
            };
            let signed = self.expr_signed(&expr);
            let mut temp = |purpose| {
                let (id, name) = self.m.temp(purpose, width, signed, float.is_none());
                self.m.variables.get_mut(&id).expect("a sample").float = float;
                name
            };
            let sample = temp("event_sample");
            let current = temp("event_current");
            let gate = match &item.iff {
                Some(_) => {
                    let (_, gate) = self.m.temp("event_gate", 1, false, false);
                    Some(gate)
                }
                None => None,
            };
            samples.push((sample, current, expr, item.edge, item.iff.clone(), gate));
        }
        ids.sort_unstable();
        ids.dedup();
        let site = self.wait_site(
            ids.into_iter()
                .map(|id| (id, CounterKind::Changes))
                .collect(),
            Vec::new(),
        );
        for (sample, _, expr, _, iff, gate) in &samples {
            self.assign(&sv::ir::LValue::Ident(sample.clone()), expr)?;
            if let (Some(iff), Some(gate)) = (iff, gate) {
                self.assign(
                    &sv::ir::LValue::Ident(gate.clone()),
                    &iff_truth(iff.clone()),
                )?;
            }
        }
        let word = u64::from(site);
        let point = self.unit_wait(word)?;
        let mut occurred: Option<sv::ir::Expr> = None;
        for (sample, current, expr, edge, _, gate) in &samples {
            self.assign(&sv::ir::LValue::Ident(current.clone()), expr)?;
            let mut item = edge_occurred(sample, current, *edge);
            if let Some(gate) = gate {
                item = binary(
                    item,
                    sv::ir::BinaryOp::LogicAnd,
                    sv::ir::Expr::Ident(gate.clone()),
                );
            }
            occurred = Some(match occurred {
                Some(any) => binary(any, sv::ir::BinaryOp::LogicOr, item),
                None => item,
            });
        }
        let occurred = occurred.ok_or_else(|| unsupported("an empty event control"))?;
        let (woken, _) = self.eval_truth(&occurred)?;
        let continuation = self.b.new_block();
        let again = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: woken,
            true_block: (continuation, Vec::new()),
            false_block: (again, Vec::new()),
        });
        // No occurrence: the values now are the samples of the next check.
        self.b.switch_to_block(again);
        for (sample, current, ..) in &samples {
            self.assign(
                &sv::ir::LValue::Ident(sample.clone()),
                &sv::ir::Expr::Ident(current.clone()),
            )?;
        }
        let never = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(never, SIRValue::new(0u8)));
        self.unit_wake_if(never, point, word)?;
        // The continuation of `unit_wake_if` is never taken.
        self.b.seal_block(SIRTerminator::Return);
        self.b.switch_to_block(continuation);
        self.unit_flush_point();
        Ok(())
    }
}

pub(super) fn binary(
    left: sv::ir::Expr,
    op: sv::ir::BinaryOp,
    right: sv::ir::Expr,
) -> sv::ir::Expr {
    sv::ir::Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

/// Whether the value of an event expression went from that of variable
/// `before` to that of variable `after` by an occurrence of `edge` (IEEE
/// 1800-2023 9.4.2): any change of the value, or an edge of its least
/// significant bit by Table 9-2, where a value of x or z is neither 0
/// nor 1.
pub(super) fn edge_occurred(before: &str, after: &str, edge: sv::ir::EventEdge) -> sv::ir::Expr {
    use sv::ir::BinaryOp::{EqCase, LogicAnd, LogicOr, NeCase};
    let ident = |name: &str| sv::ir::Expr::Ident(name.to_string());
    let bit = |name: &str| sv::ir::Expr::Select {
        expr: Box::new(ident(name)),
        msb: sv::ir::ConstExpr::Literal("0".into()),
        lsb: sv::ir::ConstExpr::Literal("0".into()),
        signed: false,
    };
    let is =
        |name: &str, op, value: &str| binary(bit(name), op, sv::ir::Expr::Literal(value.into()));
    // From `from` to anything else, or from neither to `to`.
    let toward = |from: &str, to: &str| {
        binary(
            binary(is(before, EqCase, from), LogicAnd, is(after, NeCase, from)),
            LogicOr,
            binary(
                binary(is(before, NeCase, from), LogicAnd, is(before, NeCase, to)),
                LogicAnd,
                is(after, EqCase, to),
            ),
        )
    };
    match edge {
        sv::ir::EventEdge::Any => binary(ident(before), NeCase, ident(after)),
        sv::ir::EventEdge::Pos => toward("1'b0", "1'b1"),
        sv::ir::EventEdge::Neg => toward("1'b1", "1'b0"),
        sv::ir::EventEdge::Both => binary(toward("1'b0", "1'b1"), LogicOr, toward("1'b1", "1'b0")),
    }
}

/// The most items whose `iff` reads only the frame that an event control
/// of a unit selects among by a site for each set of them (see
/// `Ff::unit_wait_event`), unless their conditions exclude each other.
pub(super) const MAX_SELECTED_ITEMS: usize = 4;

/// Whether at most one of `conditions` can hold at a time: each compares
/// the same expression for equality with a constant, the constants'
/// values all distinct, as the conditions that the analyzer makes for the
/// instances a virtual interface may denote are. `value` evaluates a
/// constant; a condition whose constant has no value makes them not
/// exclusive.
pub(super) fn exclusive_selection<'a>(
    conditions: impl Iterator<Item = &'a sv::ir::Expr>,
    value: impl Fn(&sv::ir::Expr) -> Option<i128>,
) -> bool {
    let mut compared: Option<&sv::ir::Expr> = None;
    let mut values = HashSet::default();
    for condition in conditions {
        let sv::ir::Expr::Binary {
            left,
            op: sv::ir::BinaryOp::Eq,
            right,
        } = condition
        else {
            return false;
        };
        let Some(constant) = value(right) else {
            return false;
        };
        if compared.is_some_and(|compared| compared != &**left) || !values.insert(constant) {
            return false;
        }
        compared = Some(left);
    }
    true
}

/// Whether the `iff` condition `iff` of an event item holds: a known
/// nonzero value, x and z not (IEEE 1800-2023 9.4.2.3), as one bit that a
/// kernel's gate and a unit's selection and samples alike take.
pub(in crate::lowering::ff) fn iff_truth(iff: sv::ir::Expr) -> sv::ir::Expr {
    let not = |expr| sv::ir::Expr::Unary {
        op: sv::ir::UnaryOp::LogicNot,
        expr: Box::new(expr),
    };
    binary(
        not(not(iff)),
        sv::ir::BinaryOp::EqCase,
        sv::ir::Expr::Literal("1'b1".into()),
    )
}

/// [`edge_occurred`] for the frontend's other event lowerings.
pub(in crate::lowering::ff) fn edge_occurred_expr(
    before: &str,
    after: &str,
    edge: sv::ir::EventEdge,
) -> sv::ir::Expr {
    edge_occurred(before, after, edge)
}
