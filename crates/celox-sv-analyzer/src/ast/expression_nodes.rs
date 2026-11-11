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
                ..
            } => containers::call(
                crate::system_functions::ContainerOp::Get,
                *layout,
                vec![container.clone(), index.clone()],
            ),
        }
    }

    /// A write of `value` to the place.
    pub(crate) fn store(&self, value: Expr) -> Stmt {
        Stmt::Eval(match self {
            ObjectPlace::Property { field, handle, .. } => classes::object_call(
                crate::system_functions::ObjectOp::Store(*field),
                vec![handle.clone(), value],
            ),
            ObjectPlace::Element {
                container,
                index,
                layout,
                ..
            } => containers::call(
                crate::system_functions::ContainerOp::Set,
                *layout,
                vec![container.clone(), index.clone(), value],
            ),
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
