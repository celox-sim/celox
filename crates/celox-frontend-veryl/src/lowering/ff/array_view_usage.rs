//! Which array views and runtime reads an expression needs.

use super::*;

#[derive(Default)]
struct FunctionInputUsage {
    runtime_reads: HashSet<VarId>,
    array_views: HashSet<VarId>,
}

impl<'a> FfParser<'a> {
    pub(super) fn array_access_needs_view(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
    ) -> bool {
        let Some(formal) = self.module.variables.get(&var_id) else {
            return false;
        };
        if formal.r#type.array.is_empty() {
            return false;
        }
        if select.1.is_some() {
            return true;
        }
        let Some(array_dims) = formal
            .r#type
            .array
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()
        else {
            return true;
        };
        let all_indices = index.0.iter().chain(&select.0).collect::<Vec<_>>();
        all_indices.len() != array_dims.len()
            || all_indices.iter().zip(array_dims).any(|(expr, dim)| {
                self.get_constant_value(expr)
                    .is_none_or(|value| value as usize >= dim)
            })
    }

    fn function_input_usage(
        &self,
        call: &veryl_analyzer::ir::FunctionCall,
        active_calls: &mut HashSet<VarId>,
    ) -> Option<FunctionInputUsage> {
        if !active_calls.insert(call.id) {
            return None;
        }
        let result = (|| {
            let function = self.module.functions.get(&call.id)?;
            let function_body = if let Some(index) = &call.index {
                function.get_function(index)?
            } else {
                function.get_function(&[])?
            };
            let mut usage = FunctionInputUsage::default();
            for (arg_path, _) in &call.outputs {
                let arg_id = *function_body.arg_map.get(arg_path)?;
                let expr = self
                    .extract_function_target_expr(&function_body, arg_id, &HashMap::default())
                    .ok()?;
                self.collect_function_input_usage(&expr, &mut usage, active_calls)?;
            }
            if let Some(ret_id) = function_body.ret {
                let expr = self
                    .extract_function_return_expr(&function_body, ret_id)
                    .ok()?;
                self.collect_function_input_usage(&expr, &mut usage, active_calls)?;
            }
            let formals = function_body
                .arg_map
                .values()
                .copied()
                .collect::<HashSet<_>>();
            usage
                .runtime_reads
                .retain(|var_id| formals.contains(var_id));
            usage.array_views.retain(|var_id| formals.contains(var_id));
            Some(usage)
        })();
        active_calls.remove(&call.id);
        result
    }

    fn collect_function_input_usage(
        &self,
        expr: &Expression,
        usage: &mut FunctionInputUsage,
        active_calls: &mut HashSet<VarId>,
    ) -> Option<()> {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, _) => {
                    usage.runtime_reads.insert(*var_id);
                    if self.array_access_needs_view(*var_id, index, select) {
                        usage.array_views.insert(*var_id);
                    }
                    for expr in &index.0 {
                        self.collect_function_input_usage(expr, usage, active_calls)?;
                    }
                    for expr in &select.0 {
                        self.collect_function_input_usage(expr, usage, active_calls)?;
                    }
                    if let Some((_, expr)) = &select.1 {
                        self.collect_function_input_usage(expr, usage, active_calls)?;
                    }
                }
                Factor::FunctionCall(call) => {
                    let nested_usage = self.function_input_usage(call, active_calls)?;
                    let function = self.module.functions.get(&call.id)?;
                    let function_body = if let Some(index) = &call.index {
                        function.get_function(index)?
                    } else {
                        function.get_function(&[])?
                    };
                    for (arg_path, arg_id) in &function_body.arg_map {
                        let formal = &self.module.variables[arg_id];
                        let needs_actual = if formal.r#type.array.is_empty() {
                            nested_usage.runtime_reads.contains(arg_id)
                        } else {
                            nested_usage.runtime_reads.contains(arg_id)
                                || nested_usage.array_views.contains(arg_id)
                        };
                        if needs_actual
                            && let Some(expr) = function_call_arg(&call.inputs, arg_path)
                        {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                    }
                    for dst in call.outputs.values().flatten() {
                        for expr in &dst.index.0 {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                        for expr in &dst.select.0 {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                        if let Some((_, expr)) = &dst.select.1 {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                    }
                }
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => {}
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => {
                        self.collect_function_input_usage(&input.0, usage, active_calls)?;
                    }
                    SystemFunctionKind::Readmemh(_, _)
                    | SystemFunctionKind::Display(_)
                    | SystemFunctionKind::Write(_)
                    | SystemFunctionKind::Assert { .. }
                    | SystemFunctionKind::Finish => {}
                },
                Factor::HierVariable(_) => return None,
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
            },
            Expression::Binary(lhs, _, rhs, _) => {
                self.collect_function_input_usage(lhs, usage, active_calls)?;
                self.collect_function_input_usage(rhs, usage, active_calls)?;
            }
            Expression::Unary(_, inner, _) => {
                self.collect_function_input_usage(inner, usage, active_calls)?;
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.collect_function_input_usage(cond, usage, active_calls)?;
                self.collect_function_input_usage(then_expr, usage, active_calls)?;
                self.collect_function_input_usage(else_expr, usage, active_calls)?;
            }
            Expression::Concatenation(items, _) => {
                for (expr, repeat) in items {
                    self.collect_function_input_usage(expr, usage, active_calls)?;
                    if let Some(repeat) = repeat {
                        self.collect_function_input_usage(repeat, usage, active_calls)?;
                    }
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(expr, repeat) => {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                            if let Some(repeat) = repeat {
                                self.collect_function_input_usage(repeat, usage, active_calls)?;
                            }
                        }
                        ArrayLiteralItem::Defaul(expr) => {
                            self.collect_function_input_usage(expr, usage, active_calls)?;
                        }
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, expr) in fields {
                    self.collect_function_input_usage(expr, usage, active_calls)?;
                }
            }
        }
        Some(())
    }

    pub(super) fn collect_array_views_for_expression(
        &self,
        expr: &Expression,
        candidates: &mut Vec<ArrayViewMergeCandidate>,
        candidate_indices: &mut HashMap<ArrayViewKey, usize>,
    ) {
        // A lowered decoder can contain a deep ternary chain. Traverse its
        // operands iteratively so callee analysis starts on a bounded stack.
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            let mut children = Vec::new();
            let mut collect = |expr| children.push(expr);
            match expr {
                Expression::Term(factor) => match factor.as_ref() {
                    Factor::Variable(var_id, index, select, _) => {
                        if self
                            .module
                            .variables
                            .get(var_id)
                            .is_some_and(|variable| !variable.r#type.array.is_empty())
                            && let Some(frame) = (0..self.frames.function_arg_stack.len())
                                .rev()
                                .find(|&frame| {
                                    self.frames.function_array_view_enabled_stack[frame]
                                        && self.frames.function_arg_stack[frame]
                                            .contains_key(var_id)
                                })
                        {
                            let key = ArrayViewKey {
                                frame,
                                var_id: *var_id,
                            };
                            let static_indices =
                                self.static_array_view_indices(*var_id, index, select);
                            self.record_array_view_merge_candidate(
                                key,
                                static_indices.as_deref(),
                                candidates,
                                candidate_indices,
                            );
                            if let Some(bound_expr) =
                                self.frames.function_arg_stack[frame].get(var_id)
                            {
                                self.collect_array_views_in_bound_expression(
                                    frame,
                                    *var_id,
                                    bound_expr,
                                    static_indices.as_deref(),
                                    candidates,
                                    candidate_indices,
                                );
                            }
                        }
                        for expr in &index.0 {
                            collect(expr);
                        }
                        for expr in &select.0 {
                            collect(expr);
                        }
                        if let Some((_, expr)) = &select.1 {
                            collect(expr);
                        }
                    }
                    Factor::FunctionCall(call) => {
                        let input_usage = self.function_input_usage(call, &mut HashSet::default());
                        let function_body =
                            self.module.functions.get(&call.id).and_then(|function| {
                                if let Some(index) = &call.index {
                                    function.get_function(index)
                                } else {
                                    function.get_function(&[])
                                }
                            });
                        if let (Some(input_usage), Some(function_body)) =
                            (input_usage, function_body)
                        {
                            for (arg_path, arg_id) in &function_body.arg_map {
                                let formal = &self.module.variables[arg_id];
                                let needs_actual = if formal.r#type.array.is_empty() {
                                    input_usage.runtime_reads.contains(arg_id)
                                } else {
                                    input_usage.runtime_reads.contains(arg_id)
                                        || input_usage.array_views.contains(arg_id)
                                };
                                if needs_actual
                                    && let Some(expr) = function_call_arg(&call.inputs, arg_path)
                                {
                                    collect(expr);
                                }
                            }
                        } else {
                            // Unsupported or recursive callees will fail during
                            // normal lowering; keep the conservative traversal.
                            for expr in call.inputs.values() {
                                collect(expr);
                            }
                        }
                        for dst in call.outputs.values().flatten() {
                            for expr in &dst.index.0 {
                                collect(expr);
                            }
                            for expr in &dst.select.0 {
                                collect(expr);
                            }
                            if let Some((_, expr)) = &dst.select.1 {
                                collect(expr);
                            }
                        }
                    }
                    Factor::SystemFunctionCall(call) => match &call.kind {
                        // Reflection queries are compile-time constants and do
                        // not evaluate their operand during expression lowering.
                        SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..) => {}
                        SystemFunctionKind::Clog2(input)
                        | SystemFunctionKind::Onehot(input)
                        | SystemFunctionKind::Signed(input)
                        | SystemFunctionKind::Unsigned(input) => collect(&input.0),
                        SystemFunctionKind::Readmemh(_, _)
                        | SystemFunctionKind::Display(_)
                        | SystemFunctionKind::Write(_)
                        | SystemFunctionKind::Assert { .. }
                        | SystemFunctionKind::Finish => {}
                    },
                    Factor::HierVariable(_) => {}
                    Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
                },
                Expression::Binary(lhs, _, rhs, _) => {
                    collect(lhs);
                    collect(rhs);
                }
                Expression::Unary(_, inner, _) => collect(inner),
                Expression::Ternary(cond, then_expr, else_expr, _) => {
                    collect(cond);
                    collect(then_expr);
                    collect(else_expr);
                }
                Expression::Concatenation(items, _) => {
                    for (expr, repeat) in items {
                        collect(expr);
                        if let Some(repeat) = repeat {
                            collect(repeat);
                        }
                    }
                }
                Expression::ArrayLiteral(items, _) => {
                    for item in items {
                        match item {
                            ArrayLiteralItem::Value(expr, repeat) => {
                                collect(expr);
                                if let Some(repeat) = repeat {
                                    collect(repeat);
                                }
                            }
                            ArrayLiteralItem::Defaul(expr) => collect(expr),
                        }
                    }
                }
                Expression::StructConstructor(_, fields, _) => {
                    for (_, expr) in fields {
                        collect(expr);
                    }
                }
            }
            pending.extend(children.into_iter().rev());
        }
    }

    fn static_array_view_indices(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Option<Vec<usize>> {
        if self.array_access_needs_view(var_id, index, select) {
            return None;
        }
        index
            .0
            .iter()
            .chain(&select.0)
            .map(|expr| self.get_constant_value(expr).map(|value| value as usize))
            .collect()
    }

    fn record_array_view_merge_candidate(
        &self,
        key: ArrayViewKey,
        static_indices: Option<&[usize]>,
        candidates: &mut Vec<ArrayViewMergeCandidate>,
        candidate_indices: &mut HashMap<ArrayViewKey, usize>,
    ) {
        let candidate_index = if let Some(&candidate_index) = candidate_indices.get(&key) {
            candidate_index
        } else {
            let candidate_index = candidates.len();
            candidates.push(ArrayViewMergeCandidate {
                key,
                needs_view: false,
                cached_literal_items: HashSet::default(),
            });
            candidate_indices.insert(key, candidate_index);
            candidate_index
        };
        let candidate = &mut candidates[candidate_index];
        if let Some(static_indices) = static_indices {
            if let Some(items) = self.bound_array_literal_items_at(key.frame, key.var_id)
                && let Some(array_dims) = self.module.variables[&key.var_id]
                    .r#type
                    .array
                    .iter()
                    .copied()
                    .collect::<Option<Vec<_>>>()
                && let Some(selection) =
                    self.select_array_literal_element(items, static_indices, &array_dims)
            {
                candidate.cached_literal_items.insert(selection.cache_key);
            }
        } else {
            candidate.needs_view = true;
        }
    }

    fn collect_array_views_in_bound_expression(
        &self,
        frame: usize,
        var_id: VarId,
        expr: &Expression,
        static_indices: Option<&[usize]>,
        candidates: &mut Vec<ArrayViewMergeCandidate>,
        candidate_indices: &mut HashMap<ArrayViewKey, usize>,
    ) {
        if let Expression::Term(factor) = expr
            && let Factor::Variable(bound_var_id, index, select, _) = factor.as_ref()
            && index.0.is_empty()
            && select.0.is_empty()
            && select.1.is_none()
            && let Some(source_frame) = (0..frame).rev().find(|&source_frame| {
                self.frames.function_array_view_enabled_stack[source_frame]
                    && self.frames.function_arg_stack[source_frame].contains_key(bound_var_id)
            })
            && let Some(source_expr) =
                self.frames.function_arg_stack[source_frame].get(bound_var_id)
        {
            let key = ArrayViewKey {
                frame: source_frame,
                var_id: *bound_var_id,
            };
            self.record_array_view_merge_candidate(
                key,
                static_indices,
                candidates,
                candidate_indices,
            );
            self.collect_array_views_in_bound_expression(
                source_frame,
                *bound_var_id,
                source_expr,
                static_indices,
                candidates,
                candidate_indices,
            );
        } else if let (Some(static_indices), Expression::ArrayLiteral(items, _)) =
            (static_indices, expr)
            && let Some(array_dims) = self.module.variables[&var_id]
                .r#type
                .array
                .iter()
                .copied()
                .collect::<Option<Vec<_>>>()
            && let Some(selection) =
                self.select_array_literal_element(items, static_indices, &array_dims)
        {
            self.collect_array_views_for_expression(selection.expr, candidates, candidate_indices);
        } else {
            self.collect_array_views_for_expression(expr, candidates, candidate_indices);
        }
    }
}
