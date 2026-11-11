//! Array literals bound to array formals: items, element selection and materialization.

use super::*;

#[derive(Clone)]
pub(super) struct ArrayLiteralSelection<'a> {
    pub(super) expr: &'a Expression,
    element_index: usize,
    element_count: usize,
    pub(super) cache_key: Vec<usize>,
}

impl<'a> FfParser<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn materialize_bound_array_literal_view<A>(
        &mut self,
        frame: usize,
        var_id: VarId,
        items: &[ArrayLiteralItem],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<FunctionArrayView, ParserError> {
        // Function bodies are lowered inline, so the formal's working region
        // is a call-scoped temporary. Populate it at the first access that
        // needs a complete view; control-flow joins carry its initialization
        // state and element snapshot explicitly.
        let Some(formal) = self.module.variables.get(&var_id) else {
            unreachable!("validated function argument must have a formal variable");
        };
        if formal.r#type.array.is_empty() {
            unreachable!("array literal view requires an array formal");
        }

        let array_dims: Option<Vec<usize>> = formal.r#type.array.iter().copied().collect();
        let Some(array_dims) = array_dims else {
            unreachable!("function argument shape validation resolves formal dimensions");
        };
        let layout = self.array_view_layout(var_id)?;

        let mut cached_elements = self.function_array_view_stack[frame]
            .get(&var_id)
            .map(|view| view.cached_literal_items.clone())
            .unwrap_or_default();
        let mut source_order = Vec::new();
        Self::collect_array_literal_elements_in_source_order(
            items,
            &mut Vec::new(),
            &mut source_order,
        );
        for (cache_key, selected_expr) in source_order {
            let element_count = selected_expr
                .comptime()
                .r#type
                .array
                .iter()
                .copied()
                .try_fold(1usize, |total, dim| {
                    dim.and_then(|dim| total.checked_mul(dim))
                })
                .unwrap_or_else(|| {
                    unreachable!(
                        "function argument validation resolves array literal item dimensions"
                    )
                });
            if let Some(cached) = cached_elements.get(&cache_key).cloned() {
                if let Some(initialized) = cached.initialized {
                    let array_view_candidates = self.array_view_merge_candidates([selected_expr]);
                    let array_view_params =
                        self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
                    let pre_array_views = self.function_array_view_stack.clone();
                    let evaluate_block = ir_builder.new_block();
                    let merged_elements = cached
                        .elements
                        .iter()
                        .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                        .collect::<Vec<_>>();
                    let mut merge_params = merged_elements.clone();
                    Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
                    let merge_block = ir_builder.new_block_with(merge_params);
                    let pre_defined = self.defined_ranges.clone();
                    let pre_dynamic = self.dynamic_defined_vars.clone();
                    let mut cached_args = cached.elements;
                    cached_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                    ir_builder.seal_block(SIRTerminator::Branch {
                        cond: initialized,
                        true_block: (merge_block, cached_args),
                        false_block: (evaluate_block, vec![]),
                    });
                    ir_builder.switch_to_block(evaluate_block);
                    let mut evaluated = self.evaluate_array_literal_item(
                        selected_expr,
                        element_count,
                        layout,
                        targets,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    let evaluated_defined =
                        std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
                    let evaluated_dynamic =
                        std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
                    let evaluated_array_views = self.function_array_view_stack.clone();
                    evaluated.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                    ir_builder.seal_block(SIRTerminator::Jump(merge_block, evaluated));
                    ir_builder.switch_to_block(merge_block);
                    self.function_array_view_stack = pre_array_views.clone();
                    self.install_merged_array_views(
                        &array_view_params,
                        &[&pre_array_views, &evaluated_array_views],
                    );
                    self.defined_ranges =
                        self.intersect_defined_states(pre_defined, evaluated_defined);
                    self.dynamic_defined_vars =
                        self.intersect_dynamic_vars(pre_dynamic, evaluated_dynamic);
                    cached_elements.insert(
                        cache_key,
                        FunctionArrayLiteralItemCache {
                            elements: merged_elements,
                            initialized: None,
                        },
                    );
                }
                continue;
            }
            let item_elements = self.evaluate_array_literal_item(
                selected_expr,
                element_count,
                layout,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            cached_elements.insert(
                cache_key,
                FunctionArrayLiteralItemCache {
                    elements: item_elements,
                    initialized: None,
                },
            );
        }

        let mut elements = Vec::with_capacity(layout.element_count);
        for linear_index in 0..layout.element_count {
            let mut remainder = linear_index;
            let mut coordinates = vec![0usize; array_dims.len()];
            for dimension in (0..array_dims.len()).rev() {
                coordinates[dimension] = remainder % array_dims[dimension];
                remainder /= array_dims[dimension];
            }

            let Some(selection) =
                self.select_array_literal_element(items, &coordinates, &array_dims)
            else {
                unreachable!("validated array literal covers every formal element");
            };
            let Some(selected_regs) = cached_elements.get(&selection.cache_key) else {
                unreachable!("every selected element was evaluated in literal source order");
            };
            if selected_regs.elements.len() != selection.element_count {
                unreachable!("selected array literal item has the validated element count");
            }
            let Some(&selected_reg) = selected_regs.elements.get(selection.element_index) else {
                unreachable!("selected array literal item index is in bounds");
            };
            elements.push(selected_reg);
        }

        self.store_array_view_elements(
            var_id,
            &elements,
            layout.element_width,
            convert,
            ir_builder,
        );

        Ok(FunctionArrayView {
            backing_var_id: var_id,
            elements,
            owns_backing: true,
            cached_literal_items: cached_elements,
            initialized: None,
        })
    }

    fn collect_array_literal_elements_in_source_order<'b>(
        items: &'b [ArrayLiteralItem],
        path: &mut Vec<usize>,
        elements: &mut Vec<(Vec<usize>, &'b Expression)>,
    ) {
        for (item_index, item) in items.iter().enumerate() {
            path.push(item_index);
            let expr = match item {
                ArrayLiteralItem::Value(expr, _) | ArrayLiteralItem::Defaul(expr) => expr.as_ref(),
            };
            if let Expression::ArrayLiteral(nested, _) = expr {
                Self::collect_array_literal_elements_in_source_order(nested, path, elements);
            } else {
                elements.push((path.clone(), expr));
            }
            path.pop();
        }
    }

    pub(super) fn bound_array_literal_items_at(
        &self,
        frame: usize,
        var_id: VarId,
    ) -> Option<&[ArrayLiteralItem]> {
        match self.function_arg_stack[frame].get(&var_id)? {
            Expression::ArrayLiteral(items, _) => Some(items),
            Expression::Term(factor) => {
                let Factor::Variable(bound_var_id, index, select, _) = factor.as_ref() else {
                    return None;
                };
                if !index.0.is_empty() || !select.0.is_empty() || select.1.is_some() {
                    return None;
                }
                let source_frame = (0..frame).rev().find(|&source_frame| {
                    self.function_array_view_enabled_stack[source_frame]
                        && self.function_arg_stack[source_frame].contains_key(bound_var_id)
                })?;
                self.bound_array_literal_items_at(source_frame, *bound_var_id)
            }
            _ => None,
        }
    }

    pub(super) fn array_literal_item_specs(
        &self,
        frame: usize,
        var_id: VarId,
    ) -> Vec<(Vec<usize>, usize)> {
        let Some(items) = self.bound_array_literal_items_at(frame, var_id) else {
            return Vec::new();
        };
        let mut source_order = Vec::new();
        Self::collect_array_literal_elements_in_source_order(
            items,
            &mut Vec::new(),
            &mut source_order,
        );
        source_order
            .into_iter()
            .map(|(cache_key, expr)| {
                let element_count = expr
                    .comptime()
                    .r#type
                    .array
                    .iter()
                    .copied()
                    .try_fold(1usize, |total, dim| {
                        dim.and_then(|dim| total.checked_mul(dim))
                    })
                    .unwrap_or_else(|| {
                        unreachable!(
                            "function argument validation resolves array literal item dimensions"
                        )
                    });
                (cache_key, element_count)
            })
            .collect()
    }

    pub(super) fn array_literal_item_elements_from_view(
        &self,
        frame: usize,
        var_id: VarId,
        cache_key: &[usize],
        view_elements: &[RegisterId],
        element_count: usize,
    ) -> Option<Vec<RegisterId>> {
        let items = self.bound_array_literal_items_at(frame, var_id)?;
        let array_dims = self.module.variables[&var_id]
            .r#type
            .array
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()?;
        let mut elements = vec![None; element_count];
        for (linear_index, &view_element) in view_elements.iter().enumerate() {
            let mut remainder = linear_index;
            let mut coordinates = vec![0usize; array_dims.len()];
            for dimension in (0..array_dims.len()).rev() {
                coordinates[dimension] = remainder % array_dims[dimension];
                remainder /= array_dims[dimension];
            }
            let selection = self.select_array_literal_element(items, &coordinates, &array_dims)?;
            if selection.cache_key == cache_key {
                elements[selection.element_index] = Some(view_element);
            }
        }
        elements.into_iter().collect()
    }

    pub(super) fn definite_array_literal_caches_from_view(
        &self,
        frame: usize,
        var_id: VarId,
        view_elements: &[RegisterId],
    ) -> HashMap<Vec<usize>, FunctionArrayLiteralItemCache> {
        self.array_literal_item_specs(frame, var_id)
            .into_iter()
            .filter_map(|(cache_key, element_count)| {
                let elements = self.array_literal_item_elements_from_view(
                    frame,
                    var_id,
                    &cache_key,
                    view_elements,
                    element_count,
                )?;
                Some((
                    cache_key,
                    FunctionArrayLiteralItemCache {
                        elements,
                        initialized: None,
                    },
                ))
            })
            .collect()
    }

    fn slice_array_value_element<A>(
        &self,
        reg: RegisterId,
        element_index: usize,
        element_count: usize,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        if element_count == 0 || element_index >= element_count {
            unreachable!("validated array value element index is in bounds");
        }
        if element_count == 1 {
            return reg;
        }
        let source_width = ir_builder.register(&reg).width();
        if !source_width.is_multiple_of(element_count) {
            unreachable!("validated array value has a divisible nonzero width");
        }
        let source_element_width = source_width / element_count;
        let source_type = ir_builder.register(&reg).clone();
        let element = match source_type {
            RegisterType::Logic { .. } => ir_builder.alloc_logic(source_element_width),
            RegisterType::Bit { signed, .. } => ir_builder.alloc_bit(source_element_width, signed),
        };
        // Whole unpacked-array values use PackedElements order: coordinate
        // zero starts at the low end of the register.
        let bit_offset = element_index * source_element_width;
        ir_builder.emit(SIRInstruction::Slice(
            element,
            reg,
            bit_offset,
            source_element_width,
        ));
        element
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_array_literal_item<A>(
        &mut self,
        expr: &Expression,
        element_count: usize,
        layout: ArrayViewLayout,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<RegisterId>, ParserError> {
        self.parse_expression(
            expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            (element_count == 1).then_some(layout.element_width),
        )?;
        let item_reg = self.stack.pop_back().unwrap();
        let mut elements = Vec::with_capacity(element_count);
        for element_index in 0..element_count {
            let element =
                self.slice_array_value_element(item_reg, element_index, element_count, ir_builder);
            elements.push(self.coerce_register_to_formal(
                ir_builder,
                element,
                layout.element_width,
                expr.comptime().r#type.signed,
                layout.signed,
                layout.is_2state,
            ));
        }
        Ok(elements)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn materialize_bound_array_literal_access<A>(
        &mut self,
        var_id: VarId,
        items: &[ArrayLiteralItem],
        index: &VarIndex,
        select: &VarSelect,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<bool, ParserError> {
        if self.array_access_needs_view(var_id, index, select) {
            return Ok(false);
        }
        let formal = &self.module.variables[&var_id];
        let array_dims = formal
            .r#type
            .array
            .iter()
            .copied()
            .flatten()
            .collect::<Vec<_>>();
        let all_indices = index.0.iter().chain(&select.0).collect::<Vec<_>>();
        let resolved_indices = all_indices
            .iter()
            .map(|expr| self.get_constant_value(expr).unwrap() as usize)
            .collect::<Vec<_>>();
        let Some(selection) =
            self.select_array_literal_element(items, &resolved_indices, &array_dims)
        else {
            return Ok(false);
        };
        let Some(frame) = (0..self.function_arg_stack.len()).rev().find(|&frame| {
            self.function_array_view_enabled_stack[frame]
                && self.function_arg_stack[frame].contains_key(&var_id)
        }) else {
            return Ok(false);
        };
        let cached = self.function_array_view_stack[frame]
            .get(&var_id)
            .and_then(|view| view.cached_literal_items.get(&selection.cache_key))
            .cloned();
        let item_elements = if let Some(cached) = cached {
            if let Some(initialized) = cached.initialized {
                let layout = self.array_view_layout(var_id)?;
                let array_view_candidates = self.array_view_merge_candidates([selection.expr]);
                let array_view_params =
                    self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
                let pre_array_views = self.function_array_view_stack.clone();
                let merge_elements = cached
                    .elements
                    .iter()
                    .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                    .collect::<Vec<_>>();
                let evaluate_block = ir_builder.new_block();
                let mut merge_params = merge_elements.clone();
                Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
                let merge_block = ir_builder.new_block_with(merge_params);
                let pre_defined = self.defined_ranges.clone();
                let pre_dynamic = self.dynamic_defined_vars.clone();
                let mut cached_args = cached.elements;
                cached_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: initialized,
                    true_block: (merge_block, cached_args),
                    false_block: (evaluate_block, vec![]),
                });
                ir_builder.switch_to_block(evaluate_block);
                let mut evaluated = self.evaluate_array_literal_item(
                    selection.expr,
                    selection.element_count,
                    layout,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                let evaluated_defined =
                    std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
                let evaluated_dynamic =
                    std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
                let evaluated_array_views = self.function_array_view_stack.clone();
                evaluated.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
                ir_builder.seal_block(SIRTerminator::Jump(merge_block, evaluated));
                ir_builder.switch_to_block(merge_block);
                self.function_array_view_stack = pre_array_views.clone();
                self.install_merged_array_views(
                    &array_view_params,
                    &[&pre_array_views, &evaluated_array_views],
                );
                self.defined_ranges = self.intersect_defined_states(pre_defined, evaluated_defined);
                self.dynamic_defined_vars =
                    self.intersect_dynamic_vars(pre_dynamic, evaluated_dynamic);
                self.function_array_view_stack[frame]
                    .get_mut(&var_id)
                    .unwrap()
                    .cached_literal_items
                    .insert(
                        selection.cache_key.clone(),
                        FunctionArrayLiteralItemCache {
                            elements: merge_elements.clone(),
                            initialized: None,
                        },
                    );
                merge_elements
            } else {
                cached.elements
            }
        } else if let Some((initialized, view_elements)) = self.function_array_view_stack[frame]
            .get(&var_id)
            .and_then(|view| {
                (!view.elements.is_empty()).then_some((view.initialized?, view.elements.clone()))
            })
        {
            let layout = self.array_view_layout(var_id)?;
            let array_view_candidates = self.array_view_merge_candidates([selection.expr]);
            let array_view_params =
                self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
            let pre_array_views = self.function_array_view_stack.clone();
            let mut carried = vec![None; selection.element_count];
            for (linear_index, view_element) in view_elements
                .iter()
                .copied()
                .enumerate()
                .take(layout.element_count)
            {
                let mut remainder = linear_index;
                let mut coordinates = vec![0usize; array_dims.len()];
                for dimension in (0..array_dims.len()).rev() {
                    coordinates[dimension] = remainder % array_dims[dimension];
                    remainder /= array_dims[dimension];
                }
                let Some(candidate) =
                    self.select_array_literal_element(items, &coordinates, &array_dims)
                else {
                    unreachable!("validated array literal covers every formal element");
                };
                if candidate.cache_key == selection.cache_key {
                    carried[candidate.element_index] = Some(view_element);
                }
            }
            let carried = carried
                .into_iter()
                .map(|element| {
                    element.unwrap_or_else(|| {
                        unreachable!("every array-valued literal item element maps to the formal")
                    })
                })
                .collect::<Vec<_>>();
            let merge_elements = carried
                .iter()
                .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                .collect::<Vec<_>>();
            let evaluate_block = ir_builder.new_block();
            let mut merge_params = merge_elements.clone();
            Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
            let merge_block = ir_builder.new_block_with(merge_params);
            let pre_defined = self.defined_ranges.clone();
            let pre_dynamic = self.dynamic_defined_vars.clone();
            let mut carried_args = carried;
            carried_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: initialized,
                true_block: (merge_block, carried_args),
                false_block: (evaluate_block, vec![]),
            });
            ir_builder.switch_to_block(evaluate_block);
            let mut evaluated = self.evaluate_array_literal_item(
                selection.expr,
                selection.element_count,
                layout,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            let evaluated_defined =
                std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let evaluated_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            let evaluated_array_views = self.function_array_view_stack.clone();
            evaluated.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, evaluated));
            ir_builder.switch_to_block(merge_block);
            self.function_array_view_stack = pre_array_views.clone();
            self.install_merged_array_views(
                &array_view_params,
                &[&pre_array_views, &evaluated_array_views],
            );
            self.defined_ranges = self.intersect_defined_states(pre_defined, evaluated_defined);
            self.dynamic_defined_vars = self.intersect_dynamic_vars(pre_dynamic, evaluated_dynamic);
            self.function_array_view_stack[frame]
                .get_mut(&var_id)
                .unwrap()
                .cached_literal_items
                .insert(
                    selection.cache_key.clone(),
                    FunctionArrayLiteralItemCache {
                        elements: merge_elements.clone(),
                        initialized: None,
                    },
                );
            merge_elements
        } else {
            let layout = self.array_view_layout(var_id)?;
            let selected_expr = selection.expr;
            let item_elements = self.evaluate_array_literal_item(
                selected_expr,
                selection.element_count,
                layout,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            if let Some(view) = self.function_array_view_stack[frame].get_mut(&var_id) {
                view.cached_literal_items.insert(
                    selection.cache_key.clone(),
                    FunctionArrayLiteralItemCache {
                        elements: item_elements.clone(),
                        initialized: None,
                    },
                );
            } else {
                let initialized = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(0u8)));
                self.function_array_view_stack[frame].insert(
                    var_id,
                    FunctionArrayView {
                        backing_var_id: var_id,
                        elements: Vec::new(),
                        owns_backing: true,
                        cached_literal_items: HashMap::from_iter([(
                            selection.cache_key.clone(),
                            FunctionArrayLiteralItemCache {
                                elements: item_elements.clone(),
                                initialized: None,
                            },
                        )]),
                        initialized: Some(initialized),
                    },
                );
            }
            item_elements
        };
        let Some(&selected_reg) = item_elements.get(selection.element_index) else {
            unreachable!("selected array literal item index is in bounds");
        };
        self.stack.push_back(selected_reg);
        Ok(true)
    }

    pub(super) fn select_array_literal_element<'b>(
        &self,
        items: &'b [ArrayLiteralItem],
        indices: &[usize],
        dims: &[usize],
    ) -> Option<ArrayLiteralSelection<'b>> {
        self.select_array_literal_element_inner(items, indices, dims, &mut Vec::new())
    }

    fn select_array_literal_element_inner<'b>(
        &self,
        items: &'b [ArrayLiteralItem],
        indices: &[usize],
        dims: &[usize],
        path: &mut Vec<usize>,
    ) -> Option<ArrayLiteralSelection<'b>> {
        // Function argument shape validation has already rejected unresolved
        // repeats and duplicate defaults, including in nested literals.
        let (&target_idx, rest_indices) = indices.split_first()?;
        let (_dim, rest_dims) = dims.split_first()?;

        let mut pos = 0usize;
        let mut default_expr: Option<(&Expression, usize)> = None;

        for (item_index, item) in items.iter().enumerate() {
            match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    let rep_count = if let Some(rep_expr) = repeat {
                        let Some(rep_count) = self.get_constant_value(rep_expr) else {
                            unreachable!(
                                "array literal repeat must be constant after function argument shape validation"
                            );
                        };
                        rep_count as usize
                    } else {
                        1
                    };

                    if target_idx < pos + rep_count {
                        path.push(item_index);
                        if rest_dims.is_empty() {
                            return Some(ArrayLiteralSelection {
                                expr,
                                element_index: 0,
                                element_count: 1,
                                cache_key: path.clone(),
                            });
                        }
                        return match expr.as_ref() {
                            Expression::ArrayLiteral(nested, _) => self
                                .select_array_literal_element_inner(
                                    nested,
                                    rest_indices,
                                    rest_dims,
                                    path,
                                ),
                            _ if expr.comptime().r#type.array.is_empty() => {
                                Some(ArrayLiteralSelection {
                                    expr,
                                    element_index: 0,
                                    element_count: 1,
                                    cache_key: path.clone(),
                                })
                            }
                            _ => Some(ArrayLiteralSelection {
                                expr,
                                element_index: Self::linear_array_index(rest_indices, rest_dims)?,
                                element_count: rest_dims.iter().product(),
                                cache_key: path.clone(),
                            }),
                        };
                    }
                    pos += rep_count;
                }
                ArrayLiteralItem::Defaul(expr) => {
                    if default_expr.is_some() {
                        unreachable!(
                            "array literal must have at most one default after function argument shape validation"
                        );
                    }
                    default_expr = Some((expr, item_index));
                }
            }
        }

        let (default_expr, default_index) = default_expr?;
        path.push(default_index);
        if rest_dims.is_empty() {
            return Some(ArrayLiteralSelection {
                expr: default_expr,
                element_index: 0,
                element_count: 1,
                cache_key: path.clone(),
            });
        }
        match default_expr {
            Expression::ArrayLiteral(nested, _) => {
                self.select_array_literal_element_inner(nested, rest_indices, rest_dims, path)
            }
            _ if default_expr.comptime().r#type.array.is_empty() => Some(ArrayLiteralSelection {
                expr: default_expr,
                element_index: 0,
                element_count: 1,
                cache_key: path.clone(),
            }),
            _ => Some(ArrayLiteralSelection {
                expr: default_expr,
                element_index: Self::linear_array_index(rest_indices, rest_dims)?,
                element_count: rest_dims.iter().product(),
                cache_key: path.clone(),
            }),
        }
    }

    fn linear_array_index(indices: &[usize], dims: &[usize]) -> Option<usize> {
        if indices.len() != dims.len() {
            return None;
        }
        indices
            .iter()
            .zip(dims)
            .try_fold(0usize, |linear, (&index, &dim)| {
                (index < dim)
                    .then(|| linear.checked_mul(dim)?.checked_add(index))
                    .flatten()
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn load_bound_array_literal_view<A>(
        &mut self,
        var_id: VarId,
        backing_var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let width = get_access_width(self.module, var_id, index, select)?;
        let formal = &self.module.variables[&var_id];
        let dest = if formal.r#type.is_2state() {
            ir_builder.alloc_bit(width, formal.r#type.signed)
        } else {
            ir_builder.alloc_logic(width)
        };
        let mut guard = AccessGuard::default();
        let mut offset = self.emit_guarded_offset_calc(
            var_id,
            index,
            select,
            domain,
            convert,
            sources,
            Some(&mut guard),
            ir_builder,
        )?;
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            let element_count = formal
                .r#type
                .array
                .iter()
                .flatten()
                .copied()
                .product::<usize>();
            if element_count == 0 || !width.is_multiple_of(element_count) {
                unreachable!("array view width is divisible by its nonzero element count");
            }
            offset = SIROffset::PackedElements {
                bit_offset: 0,
                element_width: width / element_count,
            };
        }
        let dest = emit_guarded_load(
            ir_builder,
            &guard,
            dest,
            || convert(backing_var_id, WORKING_REGION),
            offset,
            width,
        );
        self.stack.push_back(dest);
        Ok(())
    }
}
