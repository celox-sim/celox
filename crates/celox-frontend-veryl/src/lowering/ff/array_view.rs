//! Views of array formals: layout, construction, merging at joins and restoring after nested calls.

use super::*;

#[derive(Clone, Copy)]
pub(super) struct ArrayViewLayout {
    pub(super) element_count: usize,
    pub(super) element_width: usize,
    pub(super) signed: bool,
    pub(super) is_2state: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ArrayViewKey {
    pub(super) frame: usize,
    pub(super) var_id: VarId,
}

pub(super) struct ArrayViewMergeCandidate {
    pub(super) key: ArrayViewKey,
    pub(super) needs_view: bool,
    pub(super) cached_literal_items: HashSet<Vec<usize>>,
}

pub(super) struct ArrayViewMergeParams {
    key: ArrayViewKey,
    pub(super) initialized: RegisterId,
    pub(super) elements: Vec<RegisterId>,
    pub(super) cached_literal_items: Vec<ArrayLiteralItemMergeParams>,
}

pub(super) struct ArrayLiteralItemMergeParams {
    cache_key: Vec<usize>,
    pub(super) initialized: RegisterId,
    pub(super) elements: Vec<RegisterId>,
}

impl<'a> FfParser<'a> {
    pub(super) fn array_view_layout(&self, var_id: VarId) -> Result<ArrayViewLayout, ParserError> {
        let Some(variable) = self.module.variables.get(&var_id) else {
            unreachable!("validated function argument must have a formal variable");
        };
        if variable.r#type.array.is_empty() {
            unreachable!("array view requires an array variable");
        }
        let Some(element_count) = variable
            .r#type
            .array
            .iter()
            .copied()
            .try_fold(1usize, |total, dim| {
                dim.and_then(|dim| total.checked_mul(dim))
            })
        else {
            unreachable!("function argument validation resolves array dimensions without overflow");
        };
        let total_width = resolve_total_width(self.module, variable)?;
        if element_count == 0 || !total_width.is_multiple_of(element_count) {
            unreachable!("array has a nonzero element count that divides its width");
        }
        Ok(ArrayViewLayout {
            element_count,
            element_width: total_width / element_count,
            signed: variable.r#type.signed,
            is_2state: variable.r#type.is_2state(),
        })
    }

    pub(super) fn store_array_view_elements<A>(
        &self,
        backing_var_id: VarId,
        elements: &[RegisterId],
        element_width: usize,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        for (linear_index, &element) in elements.iter().enumerate() {
            let element_index = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                element_index,
                SIRValue::new(linear_index as u64),
            ));
            ir_builder.emit(SIRInstruction::Store(
                convert(backing_var_id, WORKING_REGION),
                SIROffset::Element {
                    index: element_index,
                    element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                element_width,
                element,
                Vec::new(),
                Vec::new(),
            ));
        }
    }

    fn materialize_converted_array_view<A>(
        &mut self,
        var_id: VarId,
        backing_var_id: VarId,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<FunctionArrayView, ParserError> {
        // A view can alias an existing backing only when both formals use the
        // same element storage representation. Otherwise create a converted
        // temporary for the callee while preserving element-wise coercion.
        let target = self.array_view_layout(var_id)?;
        let source = self.array_view_layout(backing_var_id)?;
        if target.element_count != source.element_count {
            unreachable!("function argument shape validation preserves array element count");
        }

        let mut elements = Vec::with_capacity(target.element_count);
        for linear_index in 0..target.element_count {
            let element_index = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                element_index,
                SIRValue::new(linear_index as u64),
            ));
            let source_reg = if source.is_2state {
                ir_builder.alloc_bit(source.element_width, source.signed)
            } else {
                ir_builder.alloc_logic(source.element_width)
            };
            ir_builder.emit(SIRInstruction::Load(
                source_reg,
                convert(backing_var_id, WORKING_REGION),
                SIROffset::Element {
                    index: element_index,
                    element_width: source.element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                source.element_width,
            ));
            let converted = self.coerce_register_to_formal(
                ir_builder,
                source_reg,
                target.element_width,
                source.signed,
                target.signed,
                target.is_2state,
            );
            elements.push(converted);
        }
        self.store_array_view_elements(
            var_id,
            &elements,
            target.element_width,
            convert,
            ir_builder,
        );
        Ok(FunctionArrayView {
            backing_var_id: var_id,
            elements,
            owns_backing: true,
            cached_literal_items: HashMap::default(),
            initialized: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_function_array_view_at<A>(
        &mut self,
        frame: usize,
        var_id: VarId,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Option<FunctionArrayView>, ParserError> {
        let Some(bound_expr) = self.function_arg_stack[frame].get(&var_id).cloned() else {
            return Ok(None);
        };

        let view = match bound_expr {
            Expression::ArrayLiteral(items, _)
                if !self.module.variables[&var_id].r#type.array.is_empty() =>
            {
                Some(self.materialize_bound_array_literal_view(
                    frame, var_id, &items, targets, domain, convert, sources, ir_builder,
                )?)
            }
            Expression::Term(factor) => {
                let Factor::Variable(bound_var_id, index, select, _) = factor.as_ref() else {
                    return Ok(None);
                };
                if !index.0.is_empty() || !select.0.is_empty() || select.1.is_some() {
                    return Ok(None);
                }
                let Some(source_frame) = (0..frame).rev().find(|&i| {
                    self.function_array_view_enabled_stack[i]
                        && self.function_arg_stack[i].contains_key(bound_var_id)
                }) else {
                    return Ok(None);
                };
                let Some(source_view) = self.ensure_function_array_view_at(
                    source_frame,
                    *bound_var_id,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?
                else {
                    return Ok(None);
                };
                let target = self.array_view_layout(var_id)?;
                let source = self.array_view_layout(*bound_var_id)?;
                if target.element_width == source.element_width
                    && target.is_2state == source.is_2state
                    && target.signed == source.signed
                {
                    Some(FunctionArrayView {
                        backing_var_id: source_view.backing_var_id,
                        elements: source_view.elements,
                        owns_backing: false,
                        cached_literal_items: source_view.cached_literal_items,
                        initialized: None,
                    })
                } else {
                    Some(self.materialize_converted_array_view(
                        var_id,
                        source_view.backing_var_id,
                        convert,
                        ir_builder,
                    )?)
                }
            }
            _ => None,
        };
        Ok(view)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn ensure_function_array_view_at<A>(
        &mut self,
        frame: usize,
        var_id: VarId,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Option<FunctionArrayView>, ParserError> {
        if let Some(view) = self.function_array_view_stack[frame].get(&var_id).cloned() {
            let Some(initialized) = view.initialized else {
                return Ok(Some(view));
            };

            let bound_expr = self.function_arg_stack[frame]
                .get(&var_id)
                .unwrap_or_else(|| unreachable!("an array view has a bound argument"))
                .clone();
            let array_view_candidates = self.array_view_merge_candidates([&bound_expr]);
            let array_view_params =
                self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
            let pre_array_views = self.function_array_view_stack.clone();
            let pre_defined = self.defined_ranges.clone();
            let pre_dynamic = self.dynamic_defined_vars.clone();
            let initialize_block = ir_builder.new_block();
            let carried_elements = if view.elements.is_empty() {
                let layout = self.array_view_layout(var_id)?;
                (0..layout.element_count)
                    .map(|_| {
                        let element = if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        };
                        ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                        element
                    })
                    .collect::<Vec<_>>()
            } else {
                view.elements.clone()
            };
            let merged_elements = carried_elements
                .iter()
                .map(|reg| ir_builder.alloc_reg(ir_builder.register(reg).clone()))
                .collect::<Vec<_>>();
            let mut merge_params = merged_elements.clone();
            Self::append_array_view_merge_registers(&array_view_params, &mut merge_params);
            let merge_block = ir_builder.new_block_with(merge_params);
            let mut carried_args = carried_elements;
            carried_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: initialized,
                true_block: (merge_block, carried_args),
                false_block: (initialize_block, vec![]),
            });

            ir_builder.switch_to_block(initialize_block);
            let Some(initialized_view) = self.build_function_array_view_at(
                frame, var_id, targets, domain, convert, sources, ir_builder,
            )?
            else {
                unreachable!("a conditional array view has a materializable binding");
            };
            if initialized_view.backing_var_id != view.backing_var_id
                || initialized_view.owns_backing != view.owns_backing
            {
                unreachable!("a function array view has stable backing metadata");
            }
            let initialized_defined =
                std::mem::replace(&mut self.defined_ranges, pre_defined.clone());
            let initialized_dynamic =
                std::mem::replace(&mut self.dynamic_defined_vars, pre_dynamic.clone());
            let initialized_array_views = self.function_array_view_stack.clone();
            let mut initialized_args = initialized_view.elements;
            initialized_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
            ir_builder.seal_block(SIRTerminator::Jump(merge_block, initialized_args));
            ir_builder.switch_to_block(merge_block);
            self.function_array_view_stack = pre_array_views.clone();
            self.install_merged_array_views(
                &array_view_params,
                &[&pre_array_views, &initialized_array_views],
            );
            self.defined_ranges = self.intersect_defined_states(pre_defined, initialized_defined);
            self.dynamic_defined_vars =
                self.intersect_dynamic_vars(pre_dynamic, initialized_dynamic);
            let cached_literal_items =
                self.definite_array_literal_caches_from_view(frame, var_id, &merged_elements);
            let view = FunctionArrayView {
                backing_var_id: view.backing_var_id,
                elements: merged_elements,
                owns_backing: view.owns_backing,
                cached_literal_items,
                initialized: None,
            };
            self.function_array_view_stack[frame].insert(var_id, view.clone());
            return Ok(Some(view));
        }

        let view = self.build_function_array_view_at(
            frame, var_id, targets, domain, convert, sources, ir_builder,
        )?;
        if let Some(view) = &view {
            self.function_array_view_stack[frame].insert(var_id, view.clone());
        }
        Ok(view)
    }

    pub(super) fn array_view_merge_candidates<'b>(
        &self,
        expressions: impl IntoIterator<Item = &'b Expression>,
    ) -> Vec<ArrayViewMergeCandidate> {
        let mut candidates = Vec::new();
        let mut candidate_indices = HashMap::default();
        for expr in expressions {
            self.collect_array_views_for_expression(expr, &mut candidates, &mut candidate_indices);
        }
        candidates.retain(|candidate| {
            self.function_array_view_stack[candidate.key.frame]
                .get(&candidate.key.var_id)
                .is_none_or(|view| view.initialized.is_some())
        });
        candidates
    }

    pub(super) fn alloc_array_view_merge_params<A>(
        &self,
        candidates: &[ArrayViewMergeCandidate],
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<ArrayViewMergeParams>, ParserError> {
        candidates
            .iter()
            .map(|candidate| {
                let key = candidate.key;
                let layout = self.array_view_layout(candidate.key.var_id)?;
                let initialized = ir_builder.alloc_bit(1, false);
                let existing_view = self.function_array_view_stack[key.frame].get(&key.var_id);
                let carries_view = candidate.needs_view
                    || existing_view.is_some_and(|view| !view.elements.is_empty());
                let elements = (0..if carries_view {
                    layout.element_count
                } else {
                    0
                })
                    .map(|_| {
                        if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        }
                    })
                    .collect();
                let existing_cache_keys = existing_view
                    .into_iter()
                    .flat_map(|view| view.cached_literal_items.keys())
                    .collect::<HashSet<_>>();
                let cached_literal_items = self
                    .array_literal_item_specs(key.frame, key.var_id)
                    .into_iter()
                    .filter(|(cache_key, _)| {
                        carries_view
                            || candidate.cached_literal_items.contains(cache_key)
                            || existing_cache_keys.contains(cache_key)
                    })
                    .map(|(cache_key, element_count)| ArrayLiteralItemMergeParams {
                        cache_key,
                        initialized: ir_builder.alloc_bit(1, false),
                        elements: (0..element_count)
                            .map(|_| {
                                if layout.is_2state {
                                    ir_builder.alloc_bit(layout.element_width, layout.signed)
                                } else {
                                    ir_builder.alloc_logic(layout.element_width)
                                }
                            })
                            .collect(),
                    })
                    .collect();
                Ok(ArrayViewMergeParams {
                    key,
                    initialized,
                    elements,
                    cached_literal_items,
                })
            })
            .collect()
    }

    pub(super) fn append_array_view_merge_registers(
        params: &[ArrayViewMergeParams],
        registers: &mut Vec<RegisterId>,
    ) {
        for params in params {
            registers.push(params.initialized);
            registers.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                registers.push(cached.initialized);
                registers.extend(cached.elements.iter().copied());
            }
        }
    }

    pub(super) fn array_view_state_args<A>(
        &self,
        params: &[ArrayViewMergeParams],
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<RegisterId>, ParserError> {
        let mut args = Vec::new();
        for params in params {
            if let Some(view) =
                self.function_array_view_stack[params.key.frame].get(&params.key.var_id)
            {
                let initialized = if let Some(initialized) = view.initialized {
                    initialized
                } else {
                    let initialized = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(1u8)));
                    initialized
                };
                args.push(initialized);
                if view.elements.is_empty() {
                    let layout = self.array_view_layout(params.key.var_id)?;
                    for _ in &params.elements {
                        let element = if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        };
                        ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                        args.push(element);
                    }
                } else {
                    if view.elements.len() != params.elements.len() {
                        unreachable!("a merged array view has the planned element count");
                    }
                    args.extend(view.elements.iter().copied());
                }
            } else {
                let initialized = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(0u8)));
                args.push(initialized);
                let layout = self.array_view_layout(params.key.var_id)?;
                for _ in &params.elements {
                    let element = if layout.is_2state {
                        ir_builder.alloc_bit(layout.element_width, layout.signed)
                    } else {
                        ir_builder.alloc_logic(layout.element_width)
                    };
                    ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                    args.push(element);
                }
            }
            let view = self.function_array_view_stack[params.key.frame].get(&params.key.var_id);
            for cached_params in &params.cached_literal_items {
                if let Some(cached) =
                    view.and_then(|view| view.cached_literal_items.get(&cached_params.cache_key))
                {
                    let initialized = if let Some(initialized) = cached.initialized {
                        initialized
                    } else {
                        let initialized = ir_builder.alloc_bit(1, false);
                        ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(1u8)));
                        initialized
                    };
                    args.push(initialized);
                    args.extend(cached.elements.iter().copied());
                } else if let Some(view) = view.filter(|view| !view.elements.is_empty()) {
                    let initialized = if let Some(initialized) = view.initialized {
                        initialized
                    } else {
                        let initialized = ir_builder.alloc_bit(1, false);
                        ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(1u8)));
                        initialized
                    };
                    let Some(elements) = self.array_literal_item_elements_from_view(
                        params.key.frame,
                        params.key.var_id,
                        &cached_params.cache_key,
                        &view.elements,
                        cached_params.elements.len(),
                    ) else {
                        unreachable!("a materialized literal view covers every cached item");
                    };
                    args.push(initialized);
                    args.extend(elements);
                } else {
                    let initialized = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Imm(initialized, SIRValue::new(0u8)));
                    args.push(initialized);
                    let layout = self.array_view_layout(params.key.var_id)?;
                    for _ in &cached_params.elements {
                        let element = if layout.is_2state {
                            ir_builder.alloc_bit(layout.element_width, layout.signed)
                        } else {
                            ir_builder.alloc_logic(layout.element_width)
                        };
                        ir_builder.emit(SIRInstruction::Imm(element, SIRValue::new(0u8)));
                        args.push(element);
                    }
                }
            }
        }
        Ok(args)
    }

    pub(super) fn install_merged_array_views(
        &mut self,
        params: &[ArrayViewMergeParams],
        states: &[&Vec<HashMap<VarId, FunctionArrayView>>],
    ) {
        for params in params {
            let views = states
                .iter()
                .filter_map(|state| state[params.key.frame].get(&params.key.var_id))
                .collect::<Vec<_>>();
            let Some(first) = views.first() else {
                continue;
            };
            if views.iter().any(|view| {
                view.backing_var_id != first.backing_var_id
                    || view.owns_backing != first.owns_backing
            }) {
                unreachable!("a function array view has stable backing metadata");
            }
            let cached_literal_items = params
                .cached_literal_items
                .iter()
                .map(|cached| {
                    (
                        cached.cache_key.clone(),
                        FunctionArrayLiteralItemCache {
                            elements: cached.elements.clone(),
                            initialized: Some(cached.initialized),
                        },
                    )
                })
                .collect();
            self.function_array_view_stack[params.key.frame].insert(
                params.key.var_id,
                FunctionArrayView {
                    backing_var_id: first.backing_var_id,
                    elements: params.elements.clone(),
                    owns_backing: first.owns_backing,
                    cached_literal_items,
                    initialized: Some(params.initialized),
                },
            );
        }
    }

    pub(super) fn restore_active_function_array_views<A>(
        &self,
        finished_views: &HashMap<VarId, FunctionArrayView>,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let clobbered = finished_views
            .values()
            .filter(|view| view.owns_backing && !view.elements.is_empty())
            .map(|view| view.backing_var_id)
            .collect::<HashSet<_>>();
        let active_owned_backings = self
            .function_array_view_stack
            .iter()
            .flat_map(|frame| frame.values())
            .filter(|view| view.owns_backing && !view.elements.is_empty())
            .map(|view| view.backing_var_id)
            .collect::<HashSet<_>>();
        // An owning snapshot is authoritative; restoring an alias afterward
        // could overwrite it with a duplicate conditional placeholder.
        // Older callers must be restored first so that the nearest active
        // invocation is the final snapshot left in the shared formal region.
        for frame in &self.function_array_view_stack {
            for view in frame.values() {
                if !clobbered.contains(&view.backing_var_id)
                    || view.elements.is_empty()
                    || (!view.owns_backing && active_owned_backings.contains(&view.backing_var_id))
                {
                    continue;
                }
                let layout = self.array_view_layout(view.backing_var_id)?;
                if let Some(initialized) = view.initialized {
                    let restore_block = ir_builder.new_block();
                    let merge_block = ir_builder.new_block();
                    ir_builder.seal_block(SIRTerminator::Branch {
                        cond: initialized,
                        true_block: (restore_block, vec![]),
                        false_block: (merge_block, vec![]),
                    });
                    ir_builder.switch_to_block(restore_block);
                    self.store_array_view_elements(
                        view.backing_var_id,
                        &view.elements,
                        layout.element_width,
                        convert,
                        ir_builder,
                    );
                    ir_builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
                    ir_builder.switch_to_block(merge_block);
                } else {
                    self.store_array_view_elements(
                        view.backing_var_id,
                        &view.elements,
                        layout.element_width,
                        convert,
                        ir_builder,
                    );
                }
            }
        }
        Ok(())
    }
}
