//! Reject implicit integral writes to enum variables (6.19.3, 6.19.4).
use super::*;
use celox_sv_syntax::{DimensionKind, ExprId, ExprKind};

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn check_enum_assignment(
        &mut self,
        op: &str,
        left: ExprId,
        right: ExprId,
    ) -> Converted<()> {
        if self.enum_variable(left) && (op != "=" || self.non_enum_expression(right)?) {
            return Err(AnalyzerError::ImplicitEnumAssignment);
        }
        Ok(())
    }

    /// A standalone enum variable or a scalar element of its unpacked array.
    /// A packed select has an integral type, even if its base is an enum.
    /// Member targets retain their existing checks: union members have an
    /// exception to enum assignment typing (6.19.3).
    fn enum_variable(&self, mut id: ExprId) -> bool {
        let native = self.tree.declarations();
        let mut indices = 0;
        loop {
            id = expression_binding::ungroup(id, native);
            match &native.expression(id).kind {
                ExprKind::Select {
                    expression,
                    dimension,
                } if matches!(dimension.kind, DimensionKind::Index(_)) => {
                    indices += 1;
                    id = *expression;
                }
                ExprKind::Name(name) => {
                    let name = instances::normalize_identifier(&name.spelling);
                    return self.dims.get(&name).is_some_and(|shape| {
                        shape.container.is_none()
                            && shape.unpacked.len() == indices
                            && !shape.packed_enum
                            && type_names::declared_type(shape).is_some_and(|ty| ty.is_enum)
                    });
                }
                _ => return false,
            }
        }
    }

    /// Expressions whose result is known to lose enum typing. Keep this
    /// separate from domain metadata, which is best-effort for randomization
    /// and does not represent every enum, including wide enums.
    fn non_enum_expression(&mut self, id: ExprId) -> Converted<bool> {
        let native = self.tree.declarations();
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            let id = expression_binding::ungroup(id, native);
            let integral = match &native.expression(id).kind {
                ExprKind::Literal(_)
                | ExprKind::Unary { .. }
                | ExprKind::Binary { .. }
                | ExprKind::Concatenation(_)
                | ExprKind::Replication { .. }
                | ExprKind::Streaming { .. } => true,
                ExprKind::Name(name) => {
                    let name = instances::normalize_identifier(&name.spelling);
                    if let Some(shape) = self.dims.get(&name)
                        && let Some(ty) = type_names::declared_type(shape)
                    {
                        !ty.is_enum
                            || shape.packed_enum
                            || !shape.unpacked.is_empty()
                            || shape.container.is_some()
                    } else {
                        self.dims.const_env.contains_key(&name)
                            && !self.dims.const_env.contains_key(&enum_marker(&name))
                            && !self
                                .dims
                                .get(&name)
                                .is_some_and(|shape| shape.enum_domain.is_some())
                    }
                }
                ExprKind::Cast { target, .. } => expression_binding::enum_cast_type(
                    self.tree,
                    self.session,
                    *target,
                    native,
                    &self.dims,
                )?
                .is_some_and(|ty| !ty.is_enum),
                ExprKind::Conditional {
                    then_expression,
                    else_expression,
                    ..
                } => {
                    pending.extend([*else_expression, *then_expression]);
                    false
                }
                _ => false,
            };
            if integral {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnalyzerError, analyze_source};
    use std::path::Path;

    #[test]
    fn implicit_integral_enum_writes_have_a_language_error() {
        for statement in [
            "value = 1;",
            "value = '0;",
            "value = A + 1;",
            "value = +A;",
            "value = ~A;",
            "value += 1;",
            "value -= 1;",
            "value++;",
            "--value;",
            "value = choose ? A : 1;",
            "value = number;",
            "value <= 1;",
            "value = #1 1;",
            "number = (value = 1);",
            "number = value++;",
            "value = int'(A);",
        ] {
            let source = format!(
                "module Top; typedef enum {{ A, B }} E; E value; int number; bit choose; initial begin {statement} end endmodule"
            );
            assert!(
                matches!(
                    analyze_source(&source, Path::new("enum_assignment.sv")),
                    Err(AnalyzerError::ImplicitEnumAssignment)
                ),
                "{statement}"
            );
        }
    }

    #[test]
    fn enum_checks_do_not_depend_on_a_resolved_domain() {
        for declaration in [
            "typedef enum logic [127:0] { A=0, B=1 } E; E value;",
            "enum { A, B } value;",
        ] {
            let source = format!("module Top; {declaration} initial value = 1; endmodule");
            let result = analyze_source(&source, Path::new("enum_assignment.sv"));
            assert!(
                matches!(result, Err(AnalyzerError::ImplicitEnumAssignment)),
                "{declaration}: {result:?}"
            );
        }
    }

    #[test]
    fn enum_array_elements_are_checked_but_packed_selects_are_integral() {
        let source = "module Top; typedef enum bit [1:0] { A, B } E; E values[2]; initial values[1] = 1; endmodule";
        assert!(matches!(
            analyze_source(source, Path::new("enum_assignment.sv")),
            Err(AnalyzerError::ImplicitEnumAssignment)
        ));
        analyze_source("module Top; typedef enum bit [1:0] { A, B } E; E value; initial begin value[0] = 1'b1; value[1:0] = 2'd0; end endmodule", Path::new("enum_assignment.sv")).unwrap();
    }

    #[test]
    fn enum_casts_saved_values_and_union_members_are_not_implicit_integral_writes() {
        analyze_source(
            "module Top; typedef enum bit [1:0] {A, B} E;
             E value, copy; union packed { E state; bit [1:0] bits; } overlap;
             initial begin
                 value = E'(3); copy = (value = B);
                 value = #1 E'(3);
                 overlap.state = 3;
             end endmodule",
            Path::new("enum_assignment.sv"),
        )
        .unwrap();
    }

    #[test]
    fn enum_assignment_checks_follow_lexical_variable_types() {
        let source = "module Top; typedef enum {A, B} E;
                      initial begin E local_value; local_value = 1; end endmodule";
        assert!(matches!(
            analyze_source(source, Path::new("enum_assignment.sv")),
            Err(AnalyzerError::ImplicitEnumAssignment)
        ));
        analyze_source(
            "module Top; typedef enum {A, B} E; E value;
             initial begin int value; value = 1; value++; end endmodule",
            Path::new("enum_assignment.sv"),
        )
        .unwrap();
    }

    #[test]
    fn deep_enum_conditionals_are_checked_without_recursion() {
        let expression = format!("{}1", "choose ? A : ".repeat(1024));
        let source = format!(
            "module Top; typedef enum {{A, B}} E; E value; bit choose;
             initial value = {expression}; endmodule"
        );
        assert!(matches!(
            analyze_source(&source, Path::new("enum_assignment.sv")),
            Err(AnalyzerError::ImplicitEnumAssignment)
        ));
    }

    #[test]
    fn a_packed_enum_array_is_an_integral_rhs() {
        let source = "module Top; typedef enum bit [1:0] {A, B} E;
                      E [1:0] packed_values; E value;
                      initial value = packed_values; endmodule";
        assert!(matches!(
            analyze_source(source, Path::new("enum_assignment.sv")),
            Err(AnalyzerError::ImplicitEnumAssignment)
        ));
    }

    #[test]
    fn enum_parameters_preserve_their_type_without_domain_metadata() {
        for width in [32, 65, 128] {
            let source = format!(
                "module Top; typedef enum logic [{}:0] {{A=0, B=1}} E;
                 typedef E Alias;
                 parameter E P=B; localparam Alias Q=P; E value;
                 initial begin
                     localparam E R=Q;
                     value=P; value=Q; value=R;
                 end endmodule",
                width - 1
            );
            analyze_source(&source, Path::new("enum_parameters.sv"))
                .unwrap_or_else(|error| panic!("width {width}: {error}"));
        }
    }

    #[test]
    fn explicitly_integral_parameters_are_not_enum_values() {
        for parameter in ["parameter int P=A;", "localparam int P=A;"] {
            let source = format!(
                "module Top; typedef enum logic [127:0] {{A=0, B=1}} E;
                 {parameter} E value; initial value=P; endmodule"
            );
            assert!(matches!(
                analyze_source(&source, Path::new("enum_parameters.sv")),
                Err(AnalyzerError::ImplicitEnumAssignment)
            ));
        }
    }

    #[test]
    fn untyped_integral_parameters_do_not_inherit_enum_typing() {
        // Enums are integral (6.11.1). With no type specification an
        // integral initializer gives a logic-vector parameter (6.20.2).
        for width in [32, 65, 128] {
            let enumeration = if width == 32 {
                "enum {A, B}".to_owned()
            } else {
                format!("enum logic [{}:0] {{A=0, B=1}}", width - 1)
            };
            for declaration in [
                "parameter P=A;".to_owned(),
                "localparam P=A;".to_owned(),
                "localparam P=Typed;".to_owned(),
                format!("localparam [{}:0] P=A;", width - 1),
            ] {
                let source = format!(
                    "module Top; typedef {enumeration} E; localparam E Typed=A;
                     {declaration} E value; initial value=P; endmodule"
                );
                assert!(
                    matches!(
                        analyze_source(&source, Path::new("untyped_enum_parameter.sv")),
                        Err(AnalyzerError::ImplicitEnumAssignment)
                    ),
                    "{source}"
                );
                analyze_source(
                    &source.replace("value=P;", "value=E'(P);"),
                    Path::new("cast_untyped_enum_parameter.sv"),
                )
                .unwrap();
            }
        }
    }
}
