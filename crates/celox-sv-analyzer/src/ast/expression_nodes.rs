//! Expressions, assignments and lvalues of the AST: construction and accessors.

use super::*;

impl ConstExpr {
    /// Effectful calls get distinct sites; analyzer-created copies retain them.
    pub(super) fn call(callee: Callee, args: Vec<ConstExpr>, session: &mut Session) -> Self {
        let site = callee.is_effectful().then(|| {
            session.next_call_site += 1;
            session.next_call_site - 1
        });
        ConstExpr::Function { callee, args, site }
    }
}

impl Assignment {
    pub(super) fn new(lhs: LValue, rhs: Expr) -> Self {
        Self { lhs, rhs }
    }

    pub fn lhs(&self) -> &str {
        self.lhs.name()
    }

    pub fn lhs_value(&self) -> &LValue {
        &self.lhs
    }

    pub fn rhs(&self) -> &Expr {
        &self.rhs
    }
}

pub(crate) fn lowered_object() -> ! {
    unreachable!("an object place is lowered with the statement that assigns it")
}

impl ObjectPlace {
    /// The type of the value at the place.
    pub(crate) fn value_type(&self) -> &Type {
        match self {
            ObjectPlace::Property { r#type, .. } => r#type,
            ObjectPlace::Element { element, .. } => element,
            ObjectPlace::Member { r#type, .. } => r#type,
        }
    }

    /// The expressions that select the place: the handle, or the container
    /// and the index.
    pub(crate) fn exprs_mut(&mut self) -> Vec<&mut Expr> {
        match self {
            ObjectPlace::Property { handle, .. } => vec![handle],
            ObjectPlace::Element {
                container, index, ..
            } => vec![container, index],
            ObjectPlace::Member { outer, .. } => outer.exprs_mut(),
        }
    }

    /// The expressions that select the place, read only.
    pub(crate) fn exprs(&self) -> Vec<&Expr> {
        match self {
            ObjectPlace::Property { handle, .. } => vec![handle],
            ObjectPlace::Element {
                container, index, ..
            } => vec![container, index],
            ObjectPlace::Member { outer, .. } => outer.exprs(),
        }
    }

    /// A read of the value at the place.
    pub(crate) fn load(&self) -> Expr {
        match self {
            ObjectPlace::Property { field, handle, .. } => classes::object_call(
                crate::system_functions::ObjectOp::Load(*field),
                vec![handle.clone()],
            ),
            ObjectPlace::Element {
                container,
                index,
                layout,
                record,
                ..
            } => {
                let get = containers::call(
                    crate::system_functions::ContainerOp::Get,
                    *layout,
                    vec![container.clone(), index.clone()],
                );
                match record {
                    Some(record) => classes::object_call(
                        crate::system_functions::ObjectOp::Unbox(record.clone()),
                        vec![get],
                    ),
                    None => get,
                }
            }
            ObjectPlace::Member {
                outer,
                offset,
                width,
                r#type,
                ..
            } => Expr::Select {
                expr: Box::new(outer.load()),
                msb: ConstExpr::Literal((offset + width - 1).to_string()),
                lsb: ConstExpr::Literal(offset.to_string()),
                signed: r#type.is_signed,
            },
        }
    }

    /// A write of `value` to the place.
    pub(crate) fn store(&self, value: Expr) -> Stmt {
        if let ObjectPlace::Member {
            outer,
            offset,
            width,
            outer_width,
            ..
        } = self
        {
            // The value at `outer`, read again, with the member's bits
            // replaced: its address has no side effects (see
            // `save_destination`).
            let bits = |msb: usize, lsb: usize| Expr::Select {
                expr: Box::new(outer.load()),
                msb: ConstExpr::Literal(msb.to_string()),
                lsb: ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            let mut parts = Vec::new();
            if offset + width < *outer_width {
                parts.push(bits(outer_width - 1, offset + width));
            }
            parts.push(Expr::Resize {
                expr: Box::new(value),
                width: *width,
                signed: false,
            });
            if *offset > 0 {
                parts.push(bits(offset - 1, 0));
            }
            return outer.store(Expr::Concat(parts));
        }
        Stmt::Eval(match self {
            ObjectPlace::Property { field, handle, .. } => classes::object_call(
                crate::system_functions::ObjectOp::Store(*field),
                vec![handle.clone(), value],
            ),
            ObjectPlace::Element {
                container,
                index,
                layout,
                record,
                ..
            } => containers::call(
                crate::system_functions::ContainerOp::Set,
                *layout,
                vec![
                    container.clone(),
                    index.clone(),
                    match record {
                        Some(record) => classes::object_call(
                            crate::system_functions::ObjectOp::Box(record.clone()),
                            vec![value],
                        ),
                        None => value,
                    },
                ],
            ),
            ObjectPlace::Member { .. } => unreachable!("stored above"),
        })
    }
}

impl LValue {
    /// The variable the lvalue writes, or the `OBJECT_VALUE` variable that a
    /// place is assigned through.
    pub fn name(&self) -> &str {
        match self {
            LValue::Ident(name) | LValue::Select { name, .. } => name,
            LValue::Object { .. } => OBJECT_VALUE,
        }
    }
}

impl InsideItem {
    /// The operand expressions of the item.
    pub(super) fn exprs(&self) -> Vec<&Expr> {
        match self {
            InsideItem::Value(value) => vec![value],
            InsideItem::Range { low, high } => vec![low, high],
        }
    }

    /// Rebuild the item with `f` applied to each operand.
    pub(super) fn map(self, f: &mut impl FnMut(Expr) -> Expr) -> InsideItem {
        match self {
            InsideItem::Value(value) => InsideItem::Value(f(value)),
            InsideItem::Range { low, high } => InsideItem::Range {
                low: f(low),
                high: f(high),
            },
        }
    }
}
