//! `sort` and `rsort` with a `with` expression (IEEE 1800-2023 7.12.2).
//!
//! The expression gives each element the key it is ordered by. The keys are
//! computed once, into a hidden queue beside the container, and a stable
//! insertion sort then moves the elements and their keys together, in place.
use super::*;
use celox_sv_syntax::ExprKind;
use system_functions::{ContainerKind, ContainerLayout, ContainerOp, HandleProperty, StringOp};

impl BodyBuilder<'_, '_, '_> {
    /// The statements of `a.sort with (key)` or `a.rsort with (key)` when
    /// statement expression `id` is one.
    pub(super) fn ordering_with_statement(
        &mut self,
        id: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<Stmt>>> {
        let native = self.tree.declarations();
        let ExprKind::ArrayMethodWith {
            expression: call,
            predicate,
        } = native.expression(id).kind
        else {
            return Ok(None);
        };
        let (member, arguments) = match &native.expression(call).kind {
            ExprKind::Call {
                expression,
                arguments,
            } => (*expression, arguments.as_slice()),
            _ => (call, &[][..]),
        };
        let ExprKind::Member {
            expression: base,
            separator,
            name,
        } = &native.expression(member).kind
        else {
            return Ok(None);
        };
        let descending = match name.spelling.as_str() {
            "sort" => false,
            "rsort" => true,
            _ => return Ok(None),
        };
        if separator != "." {
            return Ok(None);
        }
        let Some(target) =
            containers::native_type(*base, native, self.tree, self.session, &self.dims)?
        else {
            return Err(unsupported(
                "ordering method with expression on a fixed array",
            ));
        };
        if matches!(target.layout.kind, ContainerKind::Associative { .. }) {
            return Err(AnalyzerError::Parse(
                "an associative array has no ordering methods (IEEE 1800-2023 7.12.2)".into(),
            ));
        }
        if target.record.is_some() {
            return Err(unsupported(
                "ordering method with expression on elements held in records",
            ));
        }
        // `a.sort()` lists one empty argument.
        let arguments = match arguments {
            [argument] if argument.name.is_none() && argument.value.is_none() => &[],
            arguments => arguments,
        };
        let iterator = match arguments {
            [] => "item".to_string(),
            [argument] if argument.name.is_none() => {
                match argument.value.map(|value| &native.expression(value).kind) {
                    Some(ExprKind::Name(name)) => instances::normalize_identifier(&name.spelling),
                    _ => return Err(unsupported("ordering method iterator argument")),
                }
            }
            _ => return Err(unsupported("ordering method arguments")),
        };
        let element = target.element.clone();
        let layout = target.layout;
        let array = self.native_value(*base)?;
        // The loops read and update the array many times, so a receiver
        // read through an expression with a side effect, such as
        // `f().q.sort with (item)`, could name another array each time.
        if inlining::effectful(&array) {
            return Err(unsupported(
                "ordering method with expression on an array read through an expression with a side effect",
            ));
        }
        let item = self.declare("$celox_sort_item", element.clone());
        let (key, key_type) = self.ordering_key(predicate, &iterator, &element, &item)?;
        let key_layout = ContainerLayout {
            kind: ContainerKind::Queue { capacity: 0 },
            width: if key_type.kind() == TypeKind::String {
                32
            } else {
                casts::expr_type_from_type(&self.tree.functions, &key_type, &self.dims.const_env)
                    .map_or(32, |shape| shape.width as u32)
            },
            signed: key_type.kind() != TypeKind::String && key_type.is_signed(),
            // An integral key keeps its unknown bits, which order no element
            // (11.4.4).
            two_state: key_type.kind() == TypeKind::String,
            handle: (key_type.kind() == TypeKind::String).then_some(HandleProperty::String),
        };
        // A container is held by its handle (see `containers::with_dimensions`).
        let mut keys_type = Type::new(TypeKind::Object);
        keys_type.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal("31".into()),
            ConstExpr::Literal("0".into()),
        ));
        keys_type.container = Some(Arc::new(ContainerType {
            key: None,
            layout: key_layout,
            element: key_type.clone(),
            record: None,
        }));
        let keys = self.declare("$celox_sort_keys", keys_type);
        let index = self.declare("$celox_sort_index", int_type());
        let slot = self.declare("$celox_sort_slot", int_type());
        let held = self.declare("$celox_sort_held", element);
        let held_key = self.declare("$celox_sort_held_key", key_type.clone());

        let ident = |name: &str| Expr::Ident(name.to_string());
        let literal = |text: &str| Expr::Literal(text.to_string());
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let assign = |name: &str, rhs| Stmt::Assign {
            lhs: LValue::Ident(name.to_string()),
            rhs,
            nonblocking: false,
        };
        let size = || containers::call(ContainerOp::Size, layout, vec![array.clone()]);
        let get = |name: &str| {
            containers::call(ContainerOp::Get, layout, vec![array.clone(), ident(name)])
        };
        let key_at =
            |at: Expr| containers::call(ContainerOp::Get, key_layout, vec![ident(&keys), at]);
        let set = |at: Expr, value| {
            Stmt::Eval(containers::call(
                ContainerOp::Set,
                layout,
                vec![array.clone(), at, value],
            ))
        };
        let set_key = |at: Expr, value| {
            Stmt::Eval(containers::call(
                ContainerOp::Set,
                key_layout,
                vec![ident(&keys), at, value],
            ))
        };
        let next = |name: &str| binary(ident(name), BinaryOp::Add, literal("1"));
        // Whether the key at `slot` belongs after the held key: a stable
        // sort moves only strictly greater keys (strictly less for rsort).
        let order = if descending {
            BinaryOp::Lt
        } else {
            BinaryOp::Gt
        };
        let after = if key_type.kind() == TypeKind::String {
            binary(
                expression_binding::string_call(
                    StringOp::Compare,
                    vec![key_at(ident(&slot)), ident(&held_key)],
                ),
                order,
                literal("0"),
            )
        } else {
            binary(key_at(ident(&slot)), order, ident(&held_key))
        };
        let for_loop = |name: &str, start: &str, body| Stmt::Loop {
            kind: LoopKind::For,
            init: vec![assign(name, literal(start))],
            condition: Some(binary(ident(name), BinaryOp::Lt, size())),
            step: vec![assign(name, next(name))],
            body,
        };
        let local = |name: &str| Stmt::Local {
            name: name.to_string(),
            init: None,
        };
        Ok(Some(vec![
            local(&keys),
            local(&held_key),
            local(&held),
            local(&item),
            local(&index),
            local(&slot),
            assign(
                &keys,
                containers::call(ContainerOp::Literal, key_layout, Vec::new()),
            ),
            for_loop(
                &index,
                "0",
                vec![
                    assign(&item, get(&index)),
                    Stmt::Eval(containers::call(
                        ContainerOp::PushBack,
                        key_layout,
                        vec![ident(&keys), key],
                    )),
                ],
            ),
            for_loop(
                &index,
                "1",
                vec![
                    assign(&held, get(&index)),
                    assign(&held_key, key_at(ident(&index))),
                    assign(&slot, binary(ident(&index), BinaryOp::Sub, literal("1"))),
                    Stmt::Loop {
                        kind: LoopKind::While,
                        init: Vec::new(),
                        condition: Some(binary(
                            binary(ident(&slot), BinaryOp::Ge, literal("0")),
                            BinaryOp::LogicAnd,
                            after,
                        )),
                        step: Vec::new(),
                        body: vec![
                            set(next(&slot), get(&slot)),
                            set_key(next(&slot), key_at(ident(&slot))),
                            assign(&slot, binary(ident(&slot), BinaryOp::Sub, literal("1"))),
                        ],
                    },
                    set(next(&slot), ident(&held)),
                    set_key(next(&slot), ident(&held_key)),
                ],
            ),
        ]))
    }

    /// The key that `with` expression `predicate` gives the element in local
    /// `item`, which the expression names `iterator`, and its type: a string
    /// or an integral value of at most 64 bits.
    fn ordering_key(
        &mut self,
        predicate: celox_sv_syntax::ExprId,
        iterator: &str,
        element: &Type,
        item: &str,
    ) -> Converted<(Expr, Type)> {
        let native = self.tree.declarations();
        // The iterator is visible in the with expression only (7.12).
        let mut scope = self.dims.clone();
        let shape = dimensions_from_type(element);
        for key in parameters::binding_keys(iterator, &scope.const_env, shape.packed.len()) {
            scope.const_env.remove(&key);
        }
        scope.parameter_values.remove(iterator);
        scope.insert(iterator.to_string(), shape);
        scope
            .expression_signedness
            .insert(iterator.to_string(), element.is_signed());
        let mut key =
            expression_binding::value(predicate, native, self.tree, self.session, &scope)?;
        let string = expression_binding::string_value(&key, &scope)
            || expression_binding::native_string(
                self.tree,
                self.session,
                predicate,
                native,
                &scope,
            );
        let key_type = if string {
            key = expression_binding::string_operand(key, &scope);
            type_binding::builtin_type("string", None).expect("string is a builtin type")
        } else {
            if expression_binding::real_value(&key, &scope) {
                return Err(unsupported("ordering method with a real key"));
            }
            let bits = dimensions::bound_bits_type(self.tree, &key, &scope)
                .filter(|bits| (1..=64).contains(&bits.width))
                .ok_or_else(|| unsupported("ordering method with a key wider than 64 bits"))?;
            key = Expr::Resize {
                expr: Box::new(key),
                width: bits.width,
                signed: bits.signed,
            };
            let mut ty = Type::new(TypeKind::Logic);
            ty.is_signed = bits.signed;
            ty.packed_ranges = vec![PackedRange::new(
                ConstExpr::Literal((bits.width - 1).to_string()),
                ConstExpr::Literal("0".into()),
            )];
            ty
        };
        rename_expr_idents(&mut key, &mut |name| {
            if *name == iterator {
                *name = item.to_string();
            }
            None
        });
        self.rename_expr(&mut key);
        Ok((key, key_type))
    }
}
