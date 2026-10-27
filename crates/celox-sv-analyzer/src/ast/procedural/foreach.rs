//! Native foreach slots feed the shared dimension and lexical declaration kernel.
use super::*;
use celox_sv_syntax::{ExprKind, ForeachHeader};

pub(super) enum ForeachLoop {
    Static(String, i128, i128),
    Container {
        array: Expr,
        index: String,
        layout: ContainerLayout,
        status: Option<String>,
    },
}

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn native_foreach_indices(
        &mut self,
        header: &ForeachHeader,
    ) -> Result<Vec<ForeachLoop>, AnalyzerError> {
        let native = self.tree.declarations();
        let mut array = header.array;
        let mut unit_member = None;
        let array = loop {
            match &native.expression(array).kind {
                ExprKind::Name(name) if name.spelling == "$unit" => {
                    break unit_member.ok_or_else(|| unsupported("foreach array"))?;
                }
                ExprKind::Name(name) => break instances::normalize_identifier(&name.spelling),
                ExprKind::Member {
                    expression, name, ..
                } => {
                    unit_member = Some(instances::normalize_identifier(&name.spelling));
                    array = *expression;
                }
                ExprKind::Select { expression, .. } => array = *expression,
                _ => return Err(unsupported("foreach array")),
            }
        };
        if let Some(container) = self
            .dims
            .get(&array)
            .and_then(|shape| shape.container.clone())
        {
            let [Some(variable)] = header.variables.as_slice() else {
                return Err(unsupported("one-dimensional container foreach index"));
            };
            let mut array = Expr::Ident(array);
            self.rename_expr(&mut array);
            let mut index_type = int_type();
            let status = if let ContainerKind::Associative {
                key_width,
                signed,
                string,
            } = container.layout.kind
            {
                index_type = if string {
                    type_binding::builtin_type("string", None).expect("string is a builtin type")
                } else {
                    let mut ty = int_type();
                    ty.is_signed = signed;
                    ty.packed_ranges[0].left = ConstExpr::Literal((key_width - 1).to_string());
                    ty
                };
                Some(self.declare("$celox_foreach_status", int_type()))
            } else {
                None
            };
            let index = self.declare(
                &instances::normalize_identifier(&variable.spelling),
                index_type,
            );
            return Ok(vec![ForeachLoop::Container {
                array,
                index,
                layout: container.layout,
                status,
            }]);
        }
        let variables = header
            .variables
            .iter()
            .map(|name| {
                name.as_ref()
                    .map(|name| instances::normalize_identifier(&name.spelling))
            })
            .collect();
        self.foreach_bound_indices(array, variables)
    }
}
