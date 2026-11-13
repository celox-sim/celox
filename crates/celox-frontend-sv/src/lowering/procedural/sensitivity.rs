//! Implicit sensitivity: what a body reads, and whether an explicit event
//! list simulates like `@*`.

use super::*;

impl ProcModule<'_> {
    /// Whether `always @(items) body` simulates like `always @* body`. The
    /// body must neither suspend nor make a nonblocking assignment, directly
    /// or in a call. Then either
    /// - the items name exactly the variables the implicit event expression
    ///   of `body` has (IEEE 1800-2023 9.4.2.2), a part-select of a whole
    ///   variable naming it, or exactly the bits it has, or
    /// - the items cover every bit `body` reads, and running `body` when
    ///   none of those bits changed is unobservable: it has no side effects,
    ///   reads no variable it writes, and stores each variable at most once
    ///   per run, so it writes the values it wrote last time without
    ///   pulsing a variable for the waiters on it (4.3, 9.4.2).
    pub fn is_implicit_event_list(
        &self,
        items: &[sv::ir::EventItem],
        body: &[sv::ir::Stmt],
    ) -> bool {
        if self.suspends_or_defers(body) {
            return false;
        }
        let mut listed: HashSet<VarAtomBase<SourceVarId>> = HashSet::default();
        let mut exact = true;
        for item in items {
            if item.iff.is_some() {
                return false;
            }
            let (name, range) = match &item.expr {
                sv::ir::Expr::Ident(name) => (name, None),
                sv::ir::Expr::Select {
                    expr: base,
                    msb,
                    lsb,
                    ..
                } => {
                    let sv::ir::Expr::Ident(name) = &**base else {
                        return false;
                    };
                    let Some(range) = sv::typecheck::eval_const_expr_with_types(
                        msb,
                        self.constants,
                        self.parameter_types,
                    )
                    .zip(sv::typecheck::eval_const_expr_with_types(
                        lsb,
                        self.constants,
                        self.parameter_types,
                    ))
                    .and_then(|(msb, lsb)| {
                        packed_expr_select_offsets(base, msb, lsb, self.variables, self.name_to_id)
                    }) else {
                        return false;
                    };
                    (name, Some((range.0.min(range.1), range.0.max(range.1))))
                }
                _ => return false,
            };
            let Some(id) = self.id(name).filter(|id| !self.is_hidden(*id)) else {
                return false;
            };
            let width = self.var(id).width;
            let (lsb, msb) = range.unwrap_or((0, width - 1));
            exact &= lsb == 0 && msb == width - 1;
            listed.insert(VarAtomBase::new(id, lsb, msb));
        }
        let reads = self.implicit_reads(body, false);
        if exact && listed == reads {
            return true;
        }
        // The same bits, partitioned differently (`w[7:0] or w[31:8]` for
        // reads of every byte of `w`), are the implicit event expression too.
        let bits = |atoms: &HashSet<VarAtomBase<SourceVarId>>| -> HashSet<(SourceVarId, usize)> {
            atoms
                .iter()
                .flat_map(|atom| (atom.access.lsb..=atom.access.msb).map(move |bit| (atom.id, bit)))
                .collect()
        };
        if bits(&listed) == bits(&reads) {
            return true;
        }
        if !self.is_pure(body) {
            return false;
        }
        let mut written = HashSet::default();
        written_names(body, &mut written);
        let written: HashSet<_> = written
            .iter()
            .filter_map(|name| self.id(name))
            .filter(|id| !self.is_hidden(*id))
            .collect();
        if reads.iter().any(|read| written.contains(&read.id)) {
            return false;
        }
        if stores_twice(body, &mut HashSet::default()) {
            return false;
        }
        // Longest static prefixes: constant selects keep their bits.
        self.implicit_reads(body, true).iter().all(|read| {
            (read.access.lsb..=read.access.msb).all(|bit| {
                listed.iter().any(|item| {
                    item.id == read.id && item.access.lsb <= bit && bit <= item.access.msb
                })
            })
        })
    }

    /// Whether the functions an `always @*` calls read a variable its implicit
    /// event expression does not include: `always @*` is sensitive only to
    /// the arguments of a called function, `always_comb` to what its body
    /// reads too (IEEE 1800-2023 9.2.2.2.2), so then the two differ.
    pub fn callees_read_beyond_wildcard(&self, process: &sv::ir::CombProcess) -> bool {
        let ids = |include_functions: bool| -> HashSet<SourceVarId> {
            self.implicit_reads(process.body(), include_functions)
                .into_iter()
                .chain(self.implicit_reads(process.sensitivity_body(), include_functions))
                .map(|atom| atom.id)
                .collect()
        };
        let wildcard = ids(false);
        ids(true).iter().any(|id| !wildcard.contains(id))
    }

    /// Syntactic implicit sensitivity, including branches that constant folding
    /// will remove. `always @*` sees arguments; `always_comb` also sees function bodies.
    /// The out-of-line eligibility of subroutines counts their stores into
    /// these as observed through this same walk (see [`Self::observe_storage`]).
    pub fn implicit_reads(
        &self,
        body: &[sv::ir::Stmt],
        include_functions: bool,
    ) -> HashSet<VarAtomBase<SourceVarId>> {
        fn expression(
            m: &ProcModule<'_>,
            expr: &sv::ir::Expr,
            include_functions: bool,
            visited: &mut HashSet<String>,
            reads: &mut HashSet<VarAtomBase<SourceVarId>>,
        ) {
            let mut exprs = Vec::new();
            use sv::ir::Expr;
            match expr {
                Expr::Call { callee, args, .. } => {
                    exprs.extend(args.iter().flatten().cloned());
                    if let sv::ir::Callee::Subroutine(name) = callee
                        && let Some(subroutine) = m.subroutine(name)
                    {
                        // The defaults of the omitted arguments, once for
                        // each pattern: defaults that call each other would
                        // otherwise be read forever.
                        let omitted: String = (0..subroutine.params.len())
                            .map(|i| {
                                if args.get(i).and_then(Option::as_ref).is_none() {
                                    '1'
                                } else {
                                    '0'
                                }
                            })
                            .collect();
                        if visited.insert(format!("{name}\0defaults {omitted}")) {
                            for (i, param) in subroutine.params.iter().enumerate() {
                                if args.get(i).and_then(Option::as_ref).is_none() {
                                    exprs.extend(param.default.clone());
                                }
                            }
                        }
                        // IEEE 1800-2023 9.2.2.2.1 includes function bodies,
                        // but only the argument expressions of task calls.
                        if include_functions && !subroutine.is_task && visited.insert(name.clone())
                        {
                            statements(m, &subroutine.body, true, visited, reads);
                            statements(m, &subroutine.sensitivity_body, true, visited, reads);
                        }
                    }
                }
                Expr::Select {
                    expr: base,
                    msb,
                    lsb,
                    ..
                } => {
                    let selected = sv::typecheck::eval_const_expr_with_types(
                        msb,
                        m.constants,
                        m.parameter_types,
                    )
                    .zip(sv::typecheck::eval_const_expr_with_types(
                        lsb,
                        m.constants,
                        m.parameter_types,
                    ))
                    .and_then(|(msb, lsb)| {
                        packed_expr_select_offsets(base, msb, lsb, m.variables, m.name_to_id)
                    });
                    // @* adds variable identifiers (9.4.2.2), whereas
                    // always_comb/latch uses longest static prefixes
                    // (9.2.2.2.1), including constant selected ranges.
                    if include_functions
                        && let (Expr::Ident(name), Some((msb, lsb))) = (&**base, selected)
                        && let Some(id) = m.id(name).filter(|id| !m.is_hidden(*id))
                    {
                        reads.insert(VarAtomBase::new(id, lsb.min(msb), lsb.max(msb)));
                    } else {
                        // Dynamic selects and wildcard controls include the base.
                        exprs.push((**base).clone());
                    }
                    exprs.extend(expr_from_const_expr_with_calls(msb, true));
                    exprs.extend(expr_from_const_expr_with_calls(lsb, true));
                }
                Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
                    exprs.push((**expr).clone())
                }
                Expr::Binary { left, right, .. } => {
                    exprs.push((**left).clone());
                    exprs.push((**right).clone());
                }
                Expr::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => {
                    exprs.extend([
                        (**condition).clone(),
                        (**then_expr).clone(),
                        (**else_expr).clone(),
                    ]);
                }
                Expr::Concat(parts) => exprs.extend(parts.iter().cloned()),
                Expr::RepeatConcat { count, parts } => {
                    exprs.extend(expr_from_const_expr(count));
                    exprs.extend(parts.iter().cloned());
                }
                Expr::Inside { expr, items } => {
                    exprs.push((**expr).clone());
                    for item in items {
                        exprs.extend(item.exprs().into_iter().cloned());
                    }
                }
                Expr::Ident(name) => {
                    if let Some(id) = m
                        .id(name)
                        .filter(|id| !m.is_hidden(*id) && !m.is_assertion_control(*id))
                    {
                        reads.insert(VarAtomBase::new(id, 0, m.var(id).width - 1));
                    }
                }
                Expr::Literal(_) => {}
            }
            for expr in exprs {
                expression(m, &expr, include_functions, visited, reads);
            }
        }
        fn statements(
            m: &ProcModule<'_>,
            body: &[sv::ir::Stmt],
            include_functions: bool,
            visited: &mut HashSet<String>,
            reads: &mut HashSet<VarAtomBase<SourceVarId>>,
        ) {
            let mut exprs = Vec::new();
            let mut positions = Vec::new();
            for stmt in body {
                let _ = stmt.clone().map(
                    &mut |expr| {
                        exprs.push(expr);
                    },
                    &mut |lhs| {
                        // A position may call a function, whose arguments
                        // are read too.
                        if let sv::ir::LValue::Select { msb, lsb, .. } = lhs {
                            positions.extend(expr_from_const_expr_with_calls(&msb, true));
                            positions.extend(expr_from_const_expr_with_calls(&lsb, true));
                        }
                    },
                );
                // A call statement carries its callee outside the expression visitor.
                stmt.walk(&mut |stmt| {
                    if let sv::ir::Stmt::Call {
                        name,
                        args,
                        arg_order,
                    } = stmt
                    {
                        exprs.push(sv::ir::Expr::Call {
                            callee: sv::ir::Callee::Subroutine(name.clone()),
                            args: args.clone(),
                            arg_order: arg_order.clone(),
                        });
                    }
                });
            }
            for expr in exprs.into_iter().chain(positions) {
                expression(m, &expr, include_functions, visited, reads);
            }
        }
        let mut reads = HashSet::default();
        statements(
            self,
            body,
            include_functions,
            &mut HashSet::default(),
            &mut reads,
        );
        reads
    }
}
