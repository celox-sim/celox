//! Case statements: the match condition of each item, the chain of
//! multiplexers they select, and which items cover every selector value.

use super::*;

impl Comb<'_, '_> {
    pub(super) fn exec_case(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
        default: Option<&[sv::ir::Stmt]>,
    ) -> Result<Store, sv::AnalyzerError> {
        // Evaluate the selector once; items compare against its value.
        let selector = if self.m.calls(selector) {
            self.hoist(&mut store, frames, selector)?
        } else {
            selector.clone()
        };
        let conditions = self.case_conditions(&mut store, frames, kind, &selector, items)?;
        let coverage = self.case_coverage(&mut store, frames, kind, &selector, items)?;
        self.exec_case_chain(store, frames, &conditions, items, default, &coverage)
    }

    /// The match condition of each case item. The selector and the labels are
    /// each evaluated self-determined, then extended to the widest of them:
    /// with their sign only when every one of them is signed (IEEE 1800-2023
    /// 12.5).
    pub(super) fn case_conditions(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
    ) -> Result<Vec<Value>, sv::AnalyzerError> {
        let mut conditions = Vec::with_capacity(items.len());
        if kind == sv::ir::CaseKind::Inside {
            for item in items {
                let condition = sv::ir::Expr::Inside {
                    expr: Box::new(selector.clone()),
                    items: item
                        .labels
                        .iter()
                        .map(|label| match label {
                            sv::ir::CaseLabel::Value(value) => {
                                sv::ir::InsideItem::Value(value.clone())
                            }
                            sv::ir::CaseLabel::Range { low, high } => sv::ir::InsideItem::Range {
                                low: low.clone(),
                                high: high.clone(),
                            },
                        })
                        .collect(),
                };
                let (node, sources) = self.eval(store, frames, &condition, None)?;
                let truth = slt_truth(self.arena, node)?;
                conditions.push((truth, sources));
            }
            return Ok(conditions);
        }
        let selector_signed = self.expr_signed(selector);
        let (selector_node, selector_sources) = self.eval_mode(
            store,
            frames,
            selector,
            None,
            kind == sv::ir::CaseKind::Exact,
        )?;
        let mut labels = Vec::with_capacity(items.len());
        let mut width = self.width(selector_node);
        let mut all_signed = selector_signed;
        for item in items {
            let mut item_labels = Vec::with_capacity(item.labels.len());
            for label in &item.labels {
                let sv::ir::CaseLabel::Value(value) = label else {
                    return Err(unsupported("range label outside case inside"));
                };
                all_signed &= self.expr_signed(value);
                let (node, sources) =
                    self.eval_mode(store, frames, value, None, kind == sv::ir::CaseKind::Exact)?;
                width = width.max(self.width(node));
                item_labels.push((node, sources));
            }
            labels.push(item_labels);
        }
        // The selector and the labels are evaluated at their common width
        // and signedness (IEEE 1800-2023 12.5), like the operands of an
        // equality: an unsigned operand also turns the others' operands
        // unsigned (11.8.2).
        let selector_node = if self.width(selector_node) < width || selector_signed != all_signed {
            self.eval_mode(
                store,
                frames,
                selector,
                Some((width, all_signed)),
                kind == sv::ir::CaseKind::Exact,
            )?
            .0
        } else {
            selector_node
        };
        let selector_node = coerce_node_width(self.arena, selector_node, Some(width), all_signed)
            .map_err(slt_error)?;
        let mut item_values = items.iter().map(|item| item.labels.iter());
        for item_labels in &mut labels {
            let values = item_values.next().into_iter().flatten();
            for ((node, _), label) in item_labels.iter_mut().zip(values) {
                if let sv::ir::CaseLabel::Value(value) = label
                    && (self.width(*node) < width || self.expr_signed(value) != all_signed)
                {
                    *node = self
                        .eval_mode(
                            store,
                            frames,
                            value,
                            Some((width, all_signed)),
                            kind == sv::ir::CaseKind::Exact,
                        )?
                        .0;
                }
            }
        }
        let op = if kind == sv::ir::CaseKind::Exact {
            BinaryOp::EqCase
        } else {
            BinaryOp::EqWildcard
        };
        for item_labels in labels {
            let mut condition: Option<Value> = None;
            for (node, mut sources) in item_labels {
                let node = coerce_node_width(self.arena, node, Some(width), all_signed)
                    .map_err(slt_error)?;
                let matched = if !self.m.four_state && kind == sv::ir::CaseKind::Exact {
                    super::super::case_equality::compare(self.arena, selector_node, op, node)
                        .map_err(slt_error)?
                } else {
                    self.alloc(SLTNode::Binary(selector_node, op, node))?
                };
                let matched = if kind == sv::ir::CaseKind::X {
                    // A wildcard comparison returns X when only selector
                    // unknowns prevent a definite match. casex ignores those
                    // bits too, so only a definite zero rejects the label.
                    // IEEE 1800-2023 12.5.1.
                    let zero = self.constant(0, 1)?;
                    self.alloc(SLTNode::Binary(matched, BinaryOp::NeCase, zero))?
                } else {
                    matched
                };
                let matched = slt_truth(self.arena, matched)?;
                sources.extend(selector_sources.iter().copied());
                condition = Some(match condition {
                    None => (matched, sources),
                    Some((other, mut other_sources)) => {
                        other_sources.extend(sources);
                        (
                            slt_or(self.arena, &mut self.consts, other, matched)?,
                            other_sources,
                        )
                    }
                });
            }
            let condition = match condition {
                Some(condition) => condition,
                None => (self.constant(0, 1)?, Sources::default()),
            };
            conditions.push(condition);
        }
        Ok(conditions)
    }

    pub(super) fn exec_case_chain(
        &mut self,
        store: Store,
        frames: &[Frame],
        conditions: &[Value],
        items: &[sv::ir::CaseItem],
        default: Option<&[sv::ir::Stmt]>,
        coverage: &[bool],
    ) -> Result<Store, sv::AnalyzerError> {
        let Some(((condition, rest_conditions), (item, rest_items))) =
            conditions.split_first().zip(items.split_first())
        else {
            return match default {
                Some(default) => self.exec_block(store, frames, default),
                None => Ok(store),
            };
        };
        let (covered, rest_coverage) = coverage
            .split_first()
            .map_or((false, &[][..]), |(covered, rest)| (*covered, rest));
        match slt_bool(self.arena, &mut self.consts, condition.0)
            .or_else(|| self.known(condition.0))
        {
            Some(true) => self.exec_block(store, frames, &item.body),
            Some(false) => self.exec_case_chain(
                store,
                frames,
                rest_conditions,
                rest_items,
                default,
                rest_coverage,
            ),
            None => {
                // Once the items so far cover every two-state selector value,
                // this item is taken whenever no earlier one is; the rest of
                // the case is unreachable.
                if covered {
                    return self.exec_block(store, frames, &item.body);
                }
                self.path
                    .facts
                    .push((condition.0, true, condition.1.clone()));
                let then_store = self.exec_block(store.fork(), frames, &item.body);
                self.path.facts.pop();
                let then_store = then_store?;
                self.path
                    .facts
                    .push((condition.0, false, condition.1.clone()));
                let else_store = self.exec_case_chain(
                    store,
                    frames,
                    rest_conditions,
                    rest_items,
                    default,
                    rest_coverage,
                );
                self.path.facts.pop();
                let else_store = else_store?;
                self.merge(condition, &then_store, &else_store)
            }
        }
    }

    /// Possible values of one selector bit, as a set of 0, 1, Z, X.
    /// Follow only wiring operations: other expressions retain the original
    /// conservative domain. Bound work even for a slice of a large/deep DAG.
    pub(super) fn case_bit_values(&self, mut node: NodeId, mut bit: usize, four_state: bool) -> u8 {
        let fallback = if four_state { 0b1111 } else { 0b0011 };
        let mut budget = 256usize;
        while budget > 0 {
            budget -= 1;
            if bit >= self.width(node) {
                return fallback;
            }
            match self.arena.get(node) {
                SLTNode::Constant(value, mask, _, _) => {
                    let digit =
                        u8::from(value.bit(bit as u64)) | (u8::from(mask.bit(bit as u64)) << 1);
                    return 1 << digit;
                }
                SLTNode::Input {
                    variable, index, ..
                } => {
                    return if !self.m.four_state
                        || (index.is_empty()
                            && self
                                .m
                                .variables
                                .get(variable)
                                .is_some_and(|var| !var.is_4state))
                    {
                        0b0011
                    } else {
                        fallback
                    };
                }
                SLTNode::Unary(UnaryOp::ToTwoState, _) => return 0b0011,
                SLTNode::Unary(UnaryOp::Ident, inner) | SLTNode::Capture { expr: inner, .. } => {
                    node = *inner
                }
                SLTNode::Slice { expr, access } => {
                    let Some(offset) = bit.checked_add(access.lsb) else {
                        return fallback;
                    };
                    bit = offset;
                    node = *expr;
                }
                SLTNode::Concat(parts) => {
                    let mut selected = None;
                    // SLT concatenations store their most significant part first.
                    for (part, width) in parts.iter().rev() {
                        if budget == 0 {
                            return fallback;
                        }
                        budget -= 1;
                        if bit < *width {
                            selected = Some(*part);
                            break;
                        }
                        bit -= width;
                    }
                    let Some(part) = selected else {
                        return fallback;
                    };
                    node = part;
                }
                _ => return fallback,
            }
        }
        fallback
    }

    /// For each item, whether it and the items before it cover every
    /// possible selector value with constant labels.
    pub(super) fn case_coverage(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
    ) -> Result<Vec<bool>, sv::AnalyzerError> {
        let none = vec![false; items.len()];
        let mut probe = store.fork();
        let (selector_node, _) = self.eval_mode(
            &mut probe,
            frames,
            selector,
            None,
            kind == sv::ir::CaseKind::Exact,
        )?;
        let selector_width = self.width(selector_node);
        // A four-state selector also takes X and Z bits; only exact
        // comparisons of a narrow selector are enumerated.
        let four_state_selector = if self.m.four_state {
            !self.two_state(selector_node)
        } else {
            !super::super::case_equality::is_known(self.arena, selector_node).map_err(slt_error)?
        };
        if four_state_selector && (kind != sv::ir::CaseKind::Exact || selector_width > 6) {
            return Ok(none);
        }
        let selector_signed = self.expr_signed(selector);
        let mut item_labels = Vec::with_capacity(items.len());
        for item in items {
            let mut labels = Vec::new();
            for label in &item.labels {
                let mut constant = |this: &mut Self,
                                    expr: &sv::ir::Expr|
                 -> Result<
                    Option<(BigUint, BigUint, usize, bool)>,
                    sv::AnalyzerError,
                > {
                    let signed = this.expr_signed(expr);
                    let (node, _) = this.eval_mode(
                        &mut probe,
                        frames,
                        expr,
                        None,
                        kind == sv::ir::CaseKind::Exact,
                    )?;
                    Ok(slt_const4(this.arena, &mut this.consts, node)
                        .map(|(value, mask, width)| (value, mask, width, signed)))
                };
                match label {
                    sv::ir::CaseLabel::Value(value) => match constant(self, value)? {
                        Some(label) => labels.push(Label::Value(label)),
                        None => return Ok(none),
                    },
                    sv::ir::CaseLabel::Range { low, high } => {
                        match (constant(self, low)?, constant(self, high)?) {
                            (Some(low), Some(high)) if low.1.is_zero() && high.1.is_zero() => {
                                labels.push(Label::Range(low.0, high.0))
                            }
                            _ => return Ok(none),
                        }
                    }
                }
            }
            item_labels.push(labels);
        }
        let all_signed = selector_signed
            && item_labels
                .iter()
                .flatten()
                .all(|label| matches!(label, Label::Value((_, _, _, true))));
        let width = item_labels
            .iter()
            .flatten()
            .map(|label| match label {
                Label::Value((_, _, width, _)) => *width,
                Label::Range(..) => 0,
            })
            .fold(selector_width, usize::max);
        let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let extend = |value: &BigUint, from: usize| -> BigUint {
            if all_signed && from > 0 && value.bit(from as u64 - 1) {
                value | (&width_mask ^ ((BigUint::from(1u8) << from) - BigUint::from(1u8)))
            } else {
                value.clone()
            }
        };
        // A selector value is `(payload, mask)`.
        let matches = |label: &Label, (value, value_mask): &(BigUint, BigUint)| match label {
            Label::Value((label_value, mask, label_width, _)) => {
                let label_value = extend(label_value, *label_width);
                let mask = extend(mask, *label_width);
                if kind == sv::ir::CaseKind::Exact {
                    *value_mask == mask && *value == label_value
                } else {
                    let care = &width_mask ^ (&mask & &width_mask);
                    value_mask.is_zero() && (value & &care) == (&label_value & &care)
                }
            }
            Label::Range(low, high) => value_mask.is_zero() && value >= low && value <= high,
        };
        let mut coverage = Vec::with_capacity(items.len());
        if selector_width > 16 {
            // Too many values to enumerate; only an all-wildcard label covers them.
            let mut covered = false;
            for labels in &item_labels {
                covered |= kind != sv::ir::CaseKind::Exact
                    && labels.iter().any(|label| {
                        matches!(label, Label::Value((_, mask, label_width, _))
                            if *label_width >= width && *mask == (BigUint::from(1u8) << *label_width) - BigUint::from(1u8))
                    });
                coverage.push(covered);
            }
            return Ok(coverage);
        }
        // A concatenated fixed X/Z bit is not a free four-state input.
        // Restrict enumeration to each bit's possible values; correlations
        // between nonconstant bits remain conservatively unconstrained.
        let digit_width = if four_state_selector { 2 } else { 1 };
        let digit_mask = (1u64 << digit_width) - 1;
        let mut fixed_bits = 0u64;
        let mut required_bits = 0u64;
        for bit in 0..selector_width {
            let values = self.case_bit_values(selector_node, bit, four_state_selector);
            let shift = digit_width * bit;
            if values.is_power_of_two() {
                let digit = u64::from(values.trailing_zeros());
                if !four_state_selector && digit > 1 {
                    return Ok(none);
                }
                fixed_bits |= digit_mask << shift;
                required_bits |= digit << shift;
            } else if four_state_selector && values == 0b0011 {
                // A two-state variable bit has a free payload and a zero mask.
                fixed_bits |= 2 << shift;
            }
        }
        let mut remaining: Vec<(BigUint, BigUint)> = if four_state_selector {
            // Each bit is 0, 1, Z (payload 0, mask 1), or X (payload 1, mask 1).
            (0u64..(1u64 << (2 * selector_width)))
                .filter(|raw| raw & fixed_bits == required_bits)
                .map(|raw| {
                    let mut value = BigUint::zero();
                    let mut mask = BigUint::zero();
                    for bit in 0..selector_width {
                        let digit = (raw >> (2 * bit)) & 3;
                        if digit & 1 != 0 {
                            value |= BigUint::from(1u8) << bit;
                        }
                        if digit & 2 != 0 {
                            mask |= BigUint::from(1u8) << bit;
                        }
                    }
                    (
                        extend(&value, selector_width),
                        extend(&mask, selector_width),
                    )
                })
                .collect()
        } else {
            (0u64..(1u64 << selector_width))
                .filter(|raw| raw & fixed_bits == required_bits)
                .map(|raw| (extend(&BigUint::from(raw), selector_width), BigUint::zero()))
                .collect()
        };
        for labels in &item_labels {
            remaining.retain(|value| !labels.iter().any(|label| matches(label, value)));
            coverage.push(remaining.is_empty());
        }
        Ok(coverage)
    }
}

pub(super) enum Label {
    Value((BigUint, BigUint, usize, bool)),
    Range(BigUint, BigUint),
}
