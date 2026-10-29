//! Array locator methods (IEEE 1800-2023 7.12.1), lowered as a loop over the
//! array where the call is evaluated, so the `with` expression runs once per
//! element after the operands before it and only when the call is reached.
use super::*;
use sv::ir::{
    BinaryOp, Callee, ContainerKind, ContainerLayout, ContainerOp, Expr, LValue, Locator,
    LocatorKey, LocatorMethod, LocatorSource, LoopKind, ObjectOp, Stmt, UnaryOp,
};

fn container(op: ContainerOp, layout: ContainerLayout, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::Object(Box::new(ObjectOp::Container { op, layout })),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}

fn ident(name: &str) -> Expr {
    Expr::Ident(name.to_string())
}

fn local(name: &str, init: Option<Expr>) -> Stmt {
    Stmt::Local {
        name: name.to_string(),
        init,
    }
}

fn assign(name: &str, rhs: Expr) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(name.to_string()),
        rhs,
        nonblocking: false,
    }
}

fn binary(left: Expr, op: BinaryOp, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    }
}

fn literal(value: &str) -> Expr {
    Expr::Literal(value.to_string())
}

impl Ff<'_, '_> {
    /// The queue that locator call `locator` returns, as a temporary. The
    /// arguments are the array, the `with` expression, and the element and
    /// index locals it reads.
    pub(super) fn locator(
        &mut self,
        locator: Locator,
        args: &[Option<Expr>],
    ) -> Result<Expr, sv::AnalyzerError> {
        let [
            Some(array),
            Some(with),
            Some(Expr::Ident(item)),
            Some(Expr::Ident(index)),
        ] = args
        else {
            return Err(unsupported("invalid array locator call"));
        };
        let method = locator.method;
        let handle = |this: &mut Self, purpose: &str| {
            let (id, name) = this.m.temp(purpose, 32, false, false);
            this.m.variables.get_mut(&id).unwrap().value_kind = celox_design::ValueKind::ObjRef;
            name
        };
        let mut stmts = Vec::new();
        let result = handle(self, "locator");
        stmts.push(local(
            &result,
            Some(container(ContainerOp::Literal, locator.result, Vec::new())),
        ));
        // What a match contributes to the result.
        let found = if method.returns_index() { index } else { item };
        let push = |value: &str| {
            Stmt::Eval(container(
                ContainerOp::PushBack,
                locator.result,
                vec![ident(&result), ident(value)],
            ))
        };
        let mut body = Vec::new();
        let mut after = Vec::new();
        match locator.key {
            None => {
                let mut then_body = vec![push(found)];
                if method.first() || method.last() {
                    then_body.push(Stmt::Break);
                }
                body.push(Stmt::If {
                    condition: with.clone(),
                    then_body,
                    else_body: Vec::new(),
                });
            }
            Some(key) => {
                let value = self.locator_key(key)?;
                body.push(local(&value, Some(with.clone())));
                if matches!(method, LocatorMethod::Min | LocatorMethod::Max) {
                    let (_, flag) = self.m.temp("locator_found", 1, false, false);
                    let best = self.locator_key(key)?;
                    let chosen = self.locator_copy(found)?;
                    stmts.push(local(&flag, Some(literal("1'b0"))));
                    stmts.push(local(&best, None));
                    stmts.push(local(&chosen, None));
                    let op = if method == LocatorMethod::Min {
                        BinaryOp::Lt
                    } else {
                        BinaryOp::Gt
                    };
                    let better = match key {
                        LocatorKey::String => binary(
                            Expr::Call {
                                callee: Callee::String(sv::ir::StringOp::Compare),
                                args: vec![Some(ident(&value)), Some(ident(&best))],
                                arg_order: Vec::new(),
                            },
                            op,
                            literal("0"),
                        ),
                        LocatorKey::Integral { .. } => binary(ident(&value), op, ident(&best)),
                    };
                    body.push(Stmt::If {
                        condition: binary(
                            Expr::Unary {
                                op: UnaryOp::LogicNot,
                                expr: Box::new(ident(&flag)),
                            },
                            BinaryOp::LogicOr,
                            better,
                        ),
                        then_body: vec![
                            assign(&flag, literal("1'b1")),
                            assign(&best, ident(&value)),
                            assign(&chosen, ident(found)),
                        ],
                        else_body: Vec::new(),
                    });
                    after.push(Stmt::If {
                        condition: ident(&flag),
                        then_body: vec![push(&chosen)],
                        else_body: Vec::new(),
                    });
                } else if let (LocatorKey::Integral { width, signed }, true) =
                    (key, self.m.four_state)
                {
                    // An associative index with X or Z bits is invalid
                    // (7.8.4), so four-state values are kept in a queue and
                    // compared with case equality: each is kept once.
                    let seen_layout = ContainerLayout {
                        kind: ContainerKind::Queue { capacity: 0 },
                        width,
                        signed,
                        two_state: false,
                        handle: None,
                    };
                    let seen = handle(self, "locator_seen");
                    let (_, position) = self.m.temp("locator_seen_index", 32, true, false);
                    let (_, repeated) = self.m.temp("locator_repeated", 1, false, false);
                    stmts.push(local(
                        &seen,
                        Some(container(ContainerOp::Literal, seen_layout, Vec::new())),
                    ));
                    body.push(local(&repeated, Some(literal("1'b0"))));
                    body.push(Stmt::Loop {
                        kind: LoopKind::For,
                        init: vec![local(&position, Some(literal("0")))],
                        condition: Some(binary(
                            ident(&position),
                            BinaryOp::Lt,
                            container(ContainerOp::Size, seen_layout, vec![ident(&seen)]),
                        )),
                        step: vec![assign(
                            &position,
                            binary(ident(&position), BinaryOp::Add, literal("1")),
                        )],
                        body: vec![Stmt::If {
                            condition: binary(
                                container(
                                    ContainerOp::Get,
                                    seen_layout,
                                    vec![ident(&seen), ident(&position)],
                                ),
                                BinaryOp::EqCase,
                                ident(&value),
                            ),
                            then_body: vec![assign(&repeated, literal("1'b1")), Stmt::Break],
                            else_body: Vec::new(),
                        }],
                    });
                    body.push(Stmt::If {
                        condition: Expr::Unary {
                            op: UnaryOp::LogicNot,
                            expr: Box::new(ident(&repeated)),
                        },
                        then_body: vec![
                            Stmt::Eval(container(
                                ContainerOp::PushBack,
                                seen_layout,
                                vec![ident(&seen), ident(&value)],
                            )),
                            push(found),
                        ],
                        else_body: Vec::new(),
                    });
                } else {
                    // The values seen so far, as the keys of an associative
                    // array, so each is kept once.
                    let (key_width, signed, string) = match key {
                        LocatorKey::Integral { width, signed } => (width, signed, false),
                        LocatorKey::String => (celox_design::HANDLE_WIDTH as u32, false, true),
                    };
                    let seen_layout = ContainerLayout {
                        kind: ContainerKind::Associative {
                            key_width,
                            signed,
                            string,
                            object: false,
                        },
                        width: 1,
                        signed: false,
                        two_state: true,
                        handle: None,
                    };
                    let seen = handle(self, "locator_seen");
                    stmts.push(local(
                        &seen,
                        Some(container(ContainerOp::Literal, seen_layout, Vec::new())),
                    ));
                    body.push(Stmt::If {
                        condition: Expr::Unary {
                            op: UnaryOp::LogicNot,
                            expr: Box::new(container(
                                ContainerOp::Exists,
                                seen_layout,
                                vec![ident(&seen), ident(&value)],
                            )),
                        },
                        then_body: vec![
                            Stmt::Eval(container(
                                ContainerOp::Set,
                                seen_layout,
                                vec![ident(&seen), ident(&value), literal("1'b1")],
                            )),
                            push(found),
                        ],
                        else_body: Vec::new(),
                    });
                }
            }
        }
        let backward = method.last();
        let mut last = None;
        let (element, init, condition, step) = match locator.source {
            LocatorSource::Container(layout) => {
                // Contents are immutable: the loop reads the array as the
                // call found it.
                let source = handle(self, "locator_array");
                stmts.push(local(&source, Some(array.clone())));
                let element =
                    container(ContainerOp::Get, layout, vec![ident(&source), ident(index)]);
                if matches!(layout.kind, ContainerKind::Associative { .. }) {
                    let (_, status) = self.m.temp("locator_status", 32, true, false);
                    let traverse = |op| container(op, layout, vec![ident(&source), ident(index)]);
                    let (start, next) = if backward {
                        (ContainerOp::Last, ContainerOp::Prev)
                    } else {
                        (ContainerOp::First, ContainerOp::Next)
                    };
                    (
                        element,
                        vec![local(index, None), local(&status, Some(traverse(start)))],
                        ident(&status),
                        vec![assign(&status, traverse(next))],
                    )
                } else {
                    let size = container(ContainerOp::Size, layout, vec![ident(&source)]);
                    if backward {
                        (
                            element,
                            vec![local(
                                index,
                                Some(binary(size, BinaryOp::Sub, literal("1"))),
                            )],
                            binary(ident(index), BinaryOp::Ge, literal("0")),
                            vec![assign(
                                index,
                                binary(ident(index), BinaryOp::Sub, literal("1")),
                            )],
                        )
                    } else {
                        (
                            element,
                            vec![local(index, Some(literal("0")))],
                            binary(ident(index), BinaryOp::Lt, size),
                            vec![assign(
                                index,
                                binary(ident(index), BinaryOp::Add, literal("1")),
                            )],
                        )
                    }
                }
            }
            LocatorSource::Fixed { left, right } => {
                let (from, to) = if backward {
                    (right, left)
                } else {
                    (left, right)
                };
                let (compare, step) = if from <= to {
                    (BinaryOp::Le, BinaryOp::Add)
                } else {
                    (BinaryOp::Ge, BinaryOp::Sub)
                };
                // The `int` index stops at the last element instead of
                // stepping past a bound at the end of its range.
                let bound = |value: i64| literal(&format!("32'sh{:08x}", value as i32 as u32));
                last = Some(Stmt::If {
                    condition: binary(ident(index), BinaryOp::Eq, bound(to)),
                    then_body: vec![Stmt::Break],
                    else_body: Vec::new(),
                });
                (
                    array.clone(),
                    vec![local(index, Some(bound(from)))],
                    binary(ident(index), compare, bound(to)),
                    vec![assign(index, binary(ident(index), step, literal("1")))],
                )
            }
        };
        body.insert(0, local(item, Some(element)));
        body.extend(last);
        stmts.push(Stmt::Loop {
            kind: LoopKind::For,
            init,
            condition: Some(condition),
            step,
            body,
        });
        stmts.extend(after);
        if !self.exec_block(&stmts)? {
            return Err(unsupported("array locator method that does not complete"));
        }
        Ok(ident(&result))
    }

    /// A temporary for a value of the `with` expression of `min`, `max` or
    /// `unique`.
    fn locator_key(&mut self, key: LocatorKey) -> Result<String, sv::AnalyzerError> {
        Ok(match key {
            LocatorKey::Integral { width, signed } => {
                self.m
                    .temp("locator_key", width as usize, signed, self.m.four_state)
                    .1
            }
            LocatorKey::String => {
                let (id, name) =
                    self.m
                        .temp("locator_key", celox_design::HANDLE_WIDTH, false, false);
                self.m.variables.get_mut(&id).unwrap().value_kind = celox_design::ValueKind::StrRef;
                name
            }
        })
    }

    /// A temporary of the type of local `name`.
    fn locator_copy(&mut self, name: &str) -> Result<String, sv::AnalyzerError> {
        let original = self
            .m
            .id(name)
            .ok_or_else(|| unsupported(format!("array locator local `{name}`")))?;
        let variable = self.m.var(original).clone();
        let (copy, copy_name) = self.m.temp(
            "locator_chosen",
            variable.width,
            variable.signed,
            variable.is_4state,
        );
        self.m.variables.insert(
            copy,
            SvVariable {
                path: vec![copy_name.clone()],
                ..variable
            },
        );
        Ok(copy_name)
    }

    /// The container that unpacked array concatenation `concat` builds: a
    /// queue that takes each run of element items, as a literal, and each
    /// array item in turn, converted to the result (IEEE 1800-2023 10.10).
    /// Each step concatenates whole sequences, so the queue is copied once
    /// per item rather than once per element.
    pub(super) fn container_concat(
        &mut self,
        concat: &sv::ir::ContainerConcat,
        args: &[Option<Expr>],
    ) -> Result<Expr, sv::AnalyzerError> {
        if args.len() != concat.parts.len() || args.iter().any(Option::is_none) {
            return Err(unsupported("invalid unpacked array concatenation"));
        }
        let queue = ContainerLayout {
            kind: ContainerKind::Queue { capacity: 0 },
            ..concat.result
        };
        let mut result: Option<Expr> = None;
        let mut elements = Vec::new();
        let append = |result: Option<Expr>, item: Expr| match result {
            None => item,
            Some(result) => container(ContainerOp::Concat, queue, vec![result, item]),
        };
        for (part, arg) in concat.parts.iter().zip(args) {
            let value = arg.clone().expect("checked above");
            if part.is_none() {
                elements.push(value);
                continue;
            }
            if !elements.is_empty() {
                let run = container(ContainerOp::Literal, queue, std::mem::take(&mut elements));
                result = Some(append(result, run));
            }
            result = Some(append(result, value));
        }
        if !elements.is_empty() {
            let run = container(ContainerOp::Literal, queue, elements);
            result = Some(append(result, run));
        }
        let result = result.unwrap_or_else(|| container(ContainerOp::Literal, queue, Vec::new()));
        // The conversion makes a new container, also of a lone array item.
        self.hoist(&container(
            ContainerOp::Convert,
            concat.result,
            vec![result],
        ))
    }
}
