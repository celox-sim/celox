//! Native source parsing and explicit semantic scope projections.

use std::path::Path;

#[cfg(test)]
use fxhash::FxHashMap as HashMap;

use crate::AnalyzerError;
#[cfg(test)]
mod aggregate_types;
#[cfg(test)]
mod alias_ownership;
#[cfg(test)]
mod array_parameter_patterns;
#[cfg(test)]
mod array_parameter_types;
#[cfg(test)]
mod body_ownership;
#[cfg(test)]
mod compilation_units;
#[cfg(test)]
mod complex_bits;
#[cfg(test)]
mod constant_calls;
#[cfg(test)]
mod constant_selects;
#[cfg(test)]
mod constant_typed_patterns;
#[cfg(test)]
mod data_ownership;
#[cfg(test)]
mod datatype_bits;
#[cfg(test)]
mod dpi_ownership;
#[cfg(test)]
mod enum_constants;
#[cfg(test)]
mod foreach;
#[cfg(test)]
mod generate_ownership;
#[cfg(test)]
mod immediate_assertions;
#[cfg(test)]
mod instance_ownership;
#[cfg(test)]
mod lightweight_casts;
#[cfg(test)]
mod member_owners;
#[cfg(test)]
mod member_paths;
#[cfg(test)]
mod member_selects;
#[cfg(test)]
mod net_ownership;
#[cfg(test)]
mod override_patterns;
#[cfg(test)]
mod package_ownership;
#[cfg(test)]
mod parameter_ownership;
#[cfg(test)]
mod ports;
#[cfg(test)]
mod process_roots;
#[cfg(test)]
mod query_ownership;
mod scopes;
pub(crate) use scopes::ScopeHandle;
#[cfg(test)]
mod roots;
#[cfg(test)]
mod runtime_bits;
#[cfg(test)]
mod scalar_parameter_types;
#[cfg(test)]
mod shaped_patterns;
#[cfg(test)]
mod subroutine_ownership;
#[cfg(test)]
mod system_values;
#[cfg(test)]
mod time_ownership;
#[cfg(test)]
mod timing_controls;
#[cfg(test)]
mod typed_actual_ranges;
#[cfg(test)]
mod typed_calls;
#[cfg(test)]
mod typed_casts;
#[cfg(test)]
mod typed_concats;
#[cfg(test)]
mod typed_parameters;
#[cfg(test)]
mod typed_patterns;
#[cfg(test)]
mod typed_ranges;
#[cfg(test)]
mod vector_patterns;

/// Shared native arena with copy-on-write support for ownership regressions.
#[derive(Clone, Debug)]
pub(super) struct Shared<T: Clone>(std::sync::Arc<T>);
impl<T: Clone> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self(std::sync::Arc::new(value))
    }
}
impl<T: Clone> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> std::ops::DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        std::sync::Arc::make_mut(&mut self.0)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct ValidatedSyntax {
    declarations: Shared<celox_sv_syntax::Declarations>,
    scope: scopes::View,
}

impl ValidatedSyntax {
    pub(crate) fn scope_handle(&self) -> ScopeHandle {
        self.scope.handle
    }
    pub(crate) fn compilation_scope(&self) -> Self {
        Self {
            scope: scopes::View::default(),
            ..self.clone()
        }
    }
    pub(crate) fn root_scopes(&self) -> Result<Vec<ScopeHandle>, AnalyzerError> {
        scopes::handles(self.declarations())
    }
    pub(crate) fn scoped(&self, handle: ScopeHandle) -> Result<Self, AnalyzerError> {
        if handle == ScopeHandle::Compilation {
            return Ok(self.compilation_scope());
        }
        if handle.unit(self.declarations()).is_none() {
            return Err(AnalyzerError::Parse(
                "explicit native unit scope is required".into(),
            ));
        }
        Ok(Self {
            scope: scopes::View {
                handle,
                selected: None,
                ports: None,
            },
            ..self.clone()
        })
    }
    pub(crate) fn with_body_membership(
        &self,
        ids: Vec<celox_sv_syntax::BodyItemId>,
    ) -> Result<Self, AnalyzerError> {
        let scope = scopes::View {
            selected: Some(ids),
            ..self.scope.clone()
        };
        scope.ids(&self.declarations)?;
        Ok(Self {
            scope,
            ..self.clone()
        })
    }
    pub(crate) fn header_port_ids(&self) -> Result<Vec<usize>, AnalyzerError> {
        let unit = self.root_unit()?;
        let ids = self
            .scope
            .ports
            .clone()
            .unwrap_or_else(|| (0..unit.ports.len()).collect());
        let mut seen = fxhash::FxHashSet::default();
        if ids
            .iter()
            .any(|id| *id >= unit.ports.len() || !seen.insert(*id))
        {
            return Err(AnalyzerError::Parse(
                "native selected header membership is invalid".into(),
            ));
        }
        Ok(ids)
    }
    pub(crate) fn with_header_membership(&self, ids: Vec<usize>) -> Result<Self, AnalyzerError> {
        let view = Self {
            scope: scopes::View {
                ports: Some(ids),
                ..self.scope.clone()
            },
            ..self.clone()
        };
        view.header_port_ids()?;
        Ok(view)
    }
    pub(crate) fn native_body_membership(
        &self,
    ) -> Result<Vec<celox_sv_syntax::BodyItemId>, AnalyzerError> {
        self.scope.ids(self.declarations())
    }
    pub(crate) fn root_unit(&self) -> Result<&celox_sv_syntax::Unit, AnalyzerError> {
        self.scope
            .handle
            .unit(&self.declarations)
            .ok_or_else(|| AnalyzerError::Parse("explicit native unit scope is required".into()))
    }
    pub(crate) fn root_name(&self) -> Result<String, AnalyzerError> {
        Ok(self.root_unit()?.name.spelling.clone())
    }
    pub(crate) fn native_scope_start(&self) -> Result<usize, AnalyzerError> {
        Ok(self
            .scope
            .handle
            .unit(&self.declarations)
            .map_or(0, |unit| unit.tokens.start))
    }
    pub(crate) fn declarations(&self) -> &celox_sv_syntax::Declarations {
        &self.declarations
    }

    pub(crate) fn into_declarations(self) -> celox_sv_syntax::Declarations {
        std::sync::Arc::try_unwrap(self.declarations.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

pub(crate) fn parse_source(code: &str, path: &Path) -> Result<ValidatedSyntax, AnalyzerError> {
    let declarations =
        celox_sv_syntax::Declarations::parse(code, path).map_err(|error| match error.kind {
            celox_sv_syntax::ParseErrorKind::MalformedHeader => {
                AnalyzerError::Parse(error.to_string())
            }
            celox_sv_syntax::ParseErrorKind::UnsupportedSyntax => {
                AnalyzerError::Unsupported(error.to_string())
            }
        })?;
    let tree = ValidatedSyntax {
        declarations: declarations.into(),
        scope: scopes::View::default(),
    };
    scopes::validate_source(&tree)?;
    Ok(tree)
}

#[cfg(test)]
mod assignment_tests {
    use super::*;
    use crate::ast::{BinaryOp, ConstExpr, Expr, LValue, Source, Stmt};

    #[test]
    fn native_continuous_assignment_lists_own_order_and_operand_values() {
        let mut tree = parse_source(
            "module M(input int x,output wire[31:0] a,b);assign a=x+1,b=x+2;endmodule",
            Path::new("native-assign-list-order.sv"),
        )
        .unwrap();
        tree.declarations.assign_statements[0]
            .assignments
            .swap(0, 1);
        native_subtraction(&mut tree);
        let source = Source::from_syntax(&tree).unwrap();
        let assignments = source.modules()[0].assignments();
        assert_eq!(
            assignments.iter().map(|a| a.lhs()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert!(assignments.iter().all(|a| matches!(
            a.rhs(),
            Expr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        )));
    }

    #[test]
    fn native_continuous_assignment_identity_and_operand_coverage_are_required() {
        for defect in [
            "missing",
            "different-class",
            "missing-operand",
            "duplicate-range",
        ] {
            let mut tree = parse_source(
                "module M(input int x,output wire[31:0] a,b);assign a=x+1,b=x+2;initial;endmodule",
                Path::new("native-assign-list-coverage.sv"),
            )
            .unwrap();
            match defect {
                "missing" => {
                    for item in &mut tree.declarations.body_items {
                        if matches!(item.kind, celox_sv_syntax::BodyItemKind::Assign(_)) {
                            item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                                category: "missing assign".into(),
                                items: Vec::new(),
                            };
                        }
                    }
                }
                "different-class" => {
                    let assign = tree
                        .declarations
                        .body_items
                        .iter()
                        .find_map(|i| match i.kind {
                            celox_sv_syntax::BodyItemKind::Assign(id) => Some(id),
                            _ => None,
                        })
                        .unwrap();
                    for item in &mut tree.declarations.body_items {
                        if matches!(item.kind, celox_sv_syntax::BodyItemKind::Procedure(_)) {
                            item.kind = celox_sv_syntax::BodyItemKind::Assign(assign);
                        }
                    }
                }
                "missing-operand" => {
                    tree.declarations.assign_statements[0].assignments.pop();
                }
                _ => {
                    let first = tree.declarations.assign_statements[0].assignments[0].tokens;
                    tree.declarations.assign_statements[0].assignments[1].tokens = first;
                }
            }
            let result = Source::from_syntax(&tree);
            if defect == "missing" {
                assert!(result.unwrap().modules()[0].assignments().is_empty());
            } else if defect == "missing-operand" {
                assert_eq!(result.unwrap().modules()[0].assignments().len(), 1);
            } else {
                assert!(result.is_err(), "{defect}");
            }
        }
    }

    #[test]
    fn native_continuous_assignment_ids_preserve_selected_generate_and_loop_scopes() {
        let tree = parse_source(
            "module M(output wire[3:0] y);if(0)begin:unused assign #1 y=0;end else begin:used for(genvar i=0;i<4;i++)begin:bit_driver assign y[i]=i;end end endmodule",
            Path::new("native-assign-active-scopes.sv"),
        ).unwrap();
        let source = Source::from_syntax(&tree).unwrap();
        let assignments = source.modules()[0].assignments();
        assert_eq!(assignments.len(), 4);
        for (i, assignment) in assignments.iter().enumerate() {
            assert!(
                matches!(assignment.lhs_value(),LValue::Select{name,msb:ConstExpr::Literal(index),lsb:ConstExpr::Literal(last),..} if name=="y" && crate::typecheck::eval_const_expr(&ConstExpr::Literal(index.clone()).into(), &HashMap::default()) == Some(i as i128) && index==last),
                "{assignment:?}"
            );
            assert_eq!(assignment.rhs(), &Expr::Literal(format!("32'sd{i}")));
        }
    }

    #[test]
    fn native_continuous_assignment_delay_restrictions_preserve_captured_scopes() {
        for body in [
            "assign #1 y=0;",
            "if(1)begin:g assign #1 y=0;end",
            "checker C;assign #1 y=0;endchecker",
            "module N;assign #1 y=0;endmodule",
        ] {
            let code = format!("module M(output wire y);{body} endmodule");
            let tree = parse_source(&code, Path::new("native-assign-delay-validation.sv")).unwrap();
            let error = Source::from_syntax_module_with_parameter_overrides(
                &tree,
                "M",
                &HashMap::default(),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("delayed continuous assignment"),
                "{body}: {error}"
            );
        }
    }

    fn native_subtraction(tree: &mut ValidatedSyntax) {
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into();
            }
        }
    }
    fn assert_subtracted_index(lhs: &LValue) {
        let LValue::Select { msb, lsb, .. } = lhs else {
            panic!("{lhs:?}")
        };
        assert_eq!(msb, lsb);
        assert!(
            matches!(
                lsb,
                ConstExpr::Binary {
                    op: BinaryOp::Sub,
                    ..
                }
            ) || crate::typecheck::eval_const_expr(&lsb.clone().into(), &HashMap::default())
                == Some(0),
            "{lsb:?}"
        );
    }
    #[test]
    fn native_left_indices_drive_continuous_and_procedural_production_lowering() {
        for source in [
            "module M(input logic d,output wire[15:0] y);assign y[1+1]=d;endmodule",
            "module M(input logic[3:0] i,input logic d,output logic[15:0] y);initial y[i+1]=d;endmodule",
            "module M(input logic[3:0] i,output logic[15:0] y);initial y[i+1]++;endmodule",
        ] {
            let mut tree = parse_source(source, Path::new("native-target-ownership.sv")).unwrap();
            native_subtraction(&mut tree);
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            if let Some(assignment) = module.assignments().first() {
                assert_subtracted_index(assignment.lhs_value());
            } else {
                let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                    panic!()
                };
                assert_subtracted_index(lhs);
            }
        }
    }
    #[test]
    fn native_for_assignment_and_increment_targets_drive_production_lowering() {
        for step in ["y[i+1]+=1", "y[i+1]++"] {
            let source = format!(
                "module M(input logic[3:0] i,output logic[15:0] y);initial for(y[i+1]=0;y[i]<1;{step});endmodule"
            );
            let mut tree = parse_source(&source, Path::new("native-for-target.sv")).unwrap();
            native_subtraction(&mut tree);
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let Stmt::Loop { init, step, .. } =
                &source.modules()[0].initial_processes()[0].body()[0]
            else {
                panic!()
            };
            for assignment in init.iter().chain(step) {
                let Stmt::Assign { lhs, .. } = assignment else {
                    panic!("{assignment:?}")
                };
                assert_subtracted_index(lhs);
            }
        }
    }
    #[test]
    fn native_assignment_target_names_are_authoritative() {
        for source in [
            "module M(input logic d,output wire[15:0] y);assign y[1]=d;endmodule",
            "module M(input logic d,output logic[15:0] y);initial y[1]=d;endmodule",
        ] {
            let mut tree = parse_source(source, Path::new("native-target-guard.sv")).unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Name(name) = &mut expression.kind
                    && name.spelling == "y"
                {
                    name.spelling = "other".into();
                }
            }
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            let lhs = if source.modules()[0].assignments().is_empty() {
                let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                    panic!()
                };
                lhs
            } else {
                module.assignments()[0].lhs_value()
            };
            assert!(matches!(lhs,LValue::Select{name,..} if name=="other"));
        }
    }
    #[test]
    fn native_target_class_disagreement_never_selects_a_typed_leaf() {
        for source in [
            "module M(input logic d,output wire[15:0] y);assign y[1]=d;endmodule",
            "module M(input logic d,output logic[15:0] y);initial y[1]=d;endmodule",
        ] {
            let mut tree = parse_source(source, Path::new("native-target-class.sv")).unwrap();
            let left = if let Some(assign) = tree.declarations.assign_statements.first() {
                assign.assignments[0].left
            } else {
                tree.declarations
                    .statements
                    .iter()
                    .find_map(|statement| match statement.kind {
                        celox_sv_syntax::StatementKind::Assignment { left, .. } => Some(left),
                        _ => None,
                    })
                    .unwrap()
            };
            let tokens = tree.declarations.expression(left).tokens;
            let index = tree
                .declarations
                .expressions
                .iter()
                .position(|expression| expression.tokens == tokens)
                .unwrap();
            tree.declarations.expressions[index].kind =
                celox_sv_syntax::ExprKind::Literal("7".into());
            assert!((crate::ast::Source::from_syntax(&tree)).is_err());
        }
    }
}

#[cfg(test)]
mod call_statement_tests {
    use super::*;
    use crate::ast::{BinaryOp, Expr, Stmt};

    #[test]
    fn native_statement_callees_arguments_and_discarded_values_reach_production() {
        for (declaration, invocation) in [
            (
                "task automatic drive(input int left=1,right=2);y=left+right;endtask",
                "drive(.right(a+1),.left(a*2))",
            ),
            (
                "function automatic int drive(input int left=1,right=2);return left+right;endfunction",
                "void'(drive(.right(a+1),.left(a*2)))",
            ),
        ] {
            let source = format!(
                "module M(input int a,output int y);{declaration} initial {invocation};endmodule"
            );
            let mut tree = parse_source(&source, Path::new("native-call-statement.sv")).unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                    && operator == "*"
                {
                    *operator = "-".into();
                }
            }
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let Stmt::Call {
                name,
                args,
                arg_order,
            } = &source.modules()[0].initial_processes()[0].body()[0]
            else {
                panic!()
            };
            assert_eq!(name, "drive");
            assert_eq!(arg_order, &[1, 0]);
            assert!(matches!(
                &args[0],
                Some(Expr::Binary {
                    op: BinaryOp::Sub,
                    ..
                })
            ));
            assert!(matches!(
                &args[1],
                Some(Expr::Binary {
                    op: BinaryOp::Add,
                    ..
                })
            ));
        }
    }
    #[test]
    fn native_bare_empty_and_omitted_statement_calls_keep_binding_contract() {
        for (call, missing, order) in [
            ("drive", vec![], vec![]),
            ("drive()", vec![], vec![]),
            ("drive(1,)", vec![false, true], vec![]),
            ("drive(,.right())", vec![true, true], vec![1, 0]),
        ] {
            let source = format!(
                "module M;task drive(input int left=1,right=2);endtask initial {call};endmodule"
            );
            let tree = parse_source(&source, Path::new("native-call-omissions.sv")).unwrap();
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let Stmt::Call {
                args, arg_order, ..
            } = &source.modules()[0].initial_processes()[0].body()[0]
            else {
                panic!()
            };
            assert_eq!(
                args.iter().map(Option::is_none).collect::<Vec<_>>(),
                missing,
                "{call}"
            );
            assert_eq!(arg_order, &order, "{call}");
        }
    }
    #[test]
    fn native_statement_name_and_formal_disagreements_do_not_retry() {
        for formal in [false, true] {
            let mut tree = parse_source(
                "module M;task drive(input int left);endtask initial drive(.left(1));endmodule",
                Path::new("native-call-statement-guard.sv"),
            )
            .unwrap();
            for expression in &mut tree.declarations.expressions {
                match &mut expression.kind {
                    celox_sv_syntax::ExprKind::Name(name)
                        if !formal && name.spelling == "drive" =>
                    {
                        name.spelling = "other".into()
                    }
                    celox_sv_syntax::ExprKind::Call { arguments, .. } if formal => {
                        arguments[0].name.as_mut().unwrap().spelling = "other".into()
                    }
                    _ => {}
                }
            }
            assert!((crate::ast::Source::from_syntax(&tree)).is_err());
        }
    }
    #[test]
    fn native_discard_wrapper_type_disagreement_is_a_diagnostic() {
        let mut tree = parse_source(
            "module M;function int f;return 1;endfunction initial void'(f());endmodule",
            Path::new("native-discard-guard.sv"),
        )
        .unwrap();
        for data_type in &mut tree.declarations.types {
            if let celox_sv_syntax::DataTypeKind::Builtin(name) = &mut data_type.kind
                && name == "void"
            {
                *name = "int".into()
            }
        }
        assert!((crate::ast::Source::from_syntax(&tree)).is_err());
    }
    #[test]
    fn native_statement_callee_class_disagreement_is_a_diagnostic() {
        let mut tree = parse_source(
            "module M;task drive;endtask initial drive();endmodule",
            Path::new("native-statement-callee-class.sv"),
        )
        .unwrap();
        for expression in &mut tree.declarations.expressions {
            if matches!(&expression.kind, celox_sv_syntax::ExprKind::Name(name) if name.spelling == "drive")
            {
                expression.kind = celox_sv_syntax::ExprKind::Literal("7".into());
            }
        }
        assert!((crate::ast::Source::from_syntax(&tree)).is_err());
    }
}

#[cfg(test)]
mod constructed_expression_tests {
    use super::*;
    use crate::ast::{BinaryOp, ConstExpr, Expr};
    #[test]
    fn native_cast_width_operand_and_replication_values_drive_production() {
        let source = "module M(input logic[7:0] a,b,output logic[15:0] y,z);assign y=(8+2)'(a+b),z={(4+2){a+b}};endmodule";
        let mut tree = parse_source(source, Path::new("native-constructed-values.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into();
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let assignments = source.modules()[0].assignments();
        let Expr::Resize { expr, width, .. } = assignments[0].rhs() else {
            panic!("{:?}", assignments[0].rhs())
        };
        assert_eq!(*width, 6);
        assert!(matches!(
            &**expr,
            Expr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ));
        let Expr::RepeatConcat { count, parts } = assignments[1].rhs() else {
            panic!()
        };
        assert!(matches!(
            count,
            ConstExpr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ));
        assert!(matches!(
            &parts[0],
            Expr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ));
    }
    #[test]
    fn native_cast_target_name_disagreement_is_a_diagnostic() {
        let mut tree = parse_source("module M(input logic a,output logic[7:0] y);typedef bit[7:0] T;assign y=T'(a);endmodule", Path::new("native-cast-target-guard.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Name(name) = &mut expression.kind
                && name.spelling == "T"
            {
                name.spelling = "Other".into();
            }
        }
        assert!((crate::ast::Source::from_syntax(&tree)).is_err());
    }
}

#[cfg(test)]
mod range_tests {
    use super::*;
    use crate::ast::{ConstExpr, Expr, LValue, Stmt};
    fn bound(value: &ConstExpr) -> i128 {
        let value: crate::ir::ConstExpr = value.clone().into();
        crate::typecheck::eval_const_expr(&value, &HashMap::from_iter([("i".into(), 8)])).unwrap()
    }
    #[test]
    fn native_range_bounds_width_start_and_prefix_indices_drive_production() {
        let source = "module M(input logic[31:0] a,input logic[1:0][7:0] b,input logic[3:0] i,output logic[31:0] y,z,w,q);assign y=a[(10+2):(6+2)],z=a[(8+2)+:(4+2)],w=a[(i+2)+:(4+2)],q=b[1][(5+2):(3+2)];endmodule";
        let mut tree = parse_source(source, Path::new("native-range-values.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into()
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        for (assignment, (high, low)) in
            source.modules()[0]
                .assignments()
                .iter()
                .zip([(8, 4), (7, 6), (7, 6), (11, 9)])
        {
            let Expr::Select {
                msb, lsb, signed, ..
            } = assignment.rhs()
            else {
                panic!("{assignment:?}")
            };
            assert_eq!(bound(msb), high);
            assert_eq!(bound(lsb), low);
            assert!(!signed);
        }
    }
    #[test]
    fn native_assignment_range_bounds_start_width_and_prefix_indices_drive_production() {
        use crate::ast::{LValue, Stmt};
        for (prefix, start) in [("assign", "8"), ("initial", "i")] {
            for (target, expected) in [
                ("y[(10+2):(6+2)]".to_owned(), (8, 4)),
                (format!("y[({start}+2)+:(4+2)]"), (7, 6)),
                ("b[1][(5+2):(3+2)]".to_owned(), (11, 9)),
            ] {
                let source = format!(
                    "module M(input logic[3:0] i,output logic[31:0] y,output logic[1:0][7:0] b);{prefix} {target}=0;endmodule"
                );
                let mut tree = parse_source(&source, Path::new("native-target-range.sv")).unwrap();
                for expression in &mut tree.declarations.expressions {
                    if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                        && operator == "+"
                    {
                        *operator = "-".into()
                    }
                }
                let source = crate::ast::Source::from_syntax(&tree).unwrap();
                let module = &source.modules()[0];
                let lhs = if prefix == "assign" {
                    module.assignments()[0].lhs_value()
                } else {
                    let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                        panic!()
                    };
                    lhs
                };
                let LValue::Select {
                    msb, lsb, signed, ..
                } = lhs
                else {
                    panic!("{lhs:?}")
                };
                assert_eq!((bound(msb), bound(lsb)), expected, "{prefix} {target}");
                assert!(!signed);
            }
        }
    }
    #[test]
    fn native_assignment_range_width_errors_and_operator_are_authoritative() {
        for prefix in ["assign", "initial"] {
            let source = format!("module M(output logic[7:0] y);{prefix} y[1+:(1+1)]=0;endmodule");
            let mut tree =
                parse_source(&source, Path::new("native-target-range-error.sv")).unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                    && operator == "+"
                {
                    *operator = "-".into()
                }
            }
            let error = crate::ast::Source::from_syntax(&tree).unwrap_err();
            assert!(error.to_string().contains("width 0"), "{error}");
            let mut tree =
                parse_source(&source, Path::new("native-target-range-error.sv")).unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Select { dimension, .. } = &mut expression.kind
                    && let celox_sv_syntax::DimensionKind::Indexed { operator, .. } =
                        &mut dimension.kind
                {
                    *operator = "-:".into()
                }
            }
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            let lhs = if prefix == "assign" {
                module.assignments()[0].lhs_value()
            } else {
                let Stmt::Assign { lhs, .. } = &module.initial_processes()[0].body()[0] else {
                    panic!()
                };
                lhs
            };
            let LValue::Select { msb, lsb, .. } = lhs else {
                panic!()
            };
            assert_eq!((bound(msb), bound(lsb)), (1, 0));
        }
    }
    #[test]
    fn native_indexed_range_width_and_shape_errors_remain_explicit() {
        for width in ["0", "-1", "n"] {
            let source = format!(
                "module M(input logic[7:0] a,input logic[2:0] i,input int n,output logic[7:0] y);assign y=a[i+:{width}];endmodule"
            );
            let error =
                crate::analyze_source(&source, Path::new("native-range-errors.sv")).unwrap_err();
            assert!(
                error.to_string().contains("indexed part-select width"),
                "{error}"
            );
        }
        for (width, message) in [("1+1", "width 0"), ("1+2", "width -1")] {
            let source = format!(
                "module M(input logic[7:0] a,output logic[7:0] y);assign y=a[1+:({width})];endmodule"
            );
            let mut tree = parse_source(&source, Path::new("native-width-error.sv")).unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                    && operator == "+"
                {
                    *operator = "-".into()
                }
            }
            let error = crate::ast::Source::from_syntax(&tree).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
        let mut tree = parse_source(
            "module M(input logic[7:0] a,output logic[7:0] y);assign y=a[1+:2];endmodule",
            Path::new("native-range-shape.sv"),
        )
        .unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Select { dimension, .. } = &mut expression.kind
                && let celox_sv_syntax::DimensionKind::Indexed { operator, .. } =
                    &mut dimension.kind
            {
                *operator = "-:".into()
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let crate::ast::Expr::Select { msb, lsb, .. } = source.modules()[0].assignments()[0].rhs()
        else {
            panic!()
        };
        assert_eq!(
            (msb, lsb),
            (
                &crate::ast::ConstExpr::Literal("1".into()),
                &crate::ast::ConstExpr::Literal("0".into())
            )
        );
    }
}

#[cfg(test)]
mod inside_tests {
    use super::*;
    use crate::ast::{BinaryOp, Expr, InsideItem};
    #[test]
    fn native_membership_operand_values_and_range_endpoints_drive_production() {
        let mut tree = parse_source("module M(input int a,b,c,d,output logic y);assign y=(a+1) inside {b+2,[c+3:d+4],a inside {b+5}};endmodule",Path::new("native-inside-ownership.sv")).unwrap();
        for expression in &mut tree.declarations.expressions {
            if let celox_sv_syntax::ExprKind::Binary { operator, .. } = &mut expression.kind
                && operator == "+"
            {
                *operator = "-".into()
            }
        }
        let source = crate::ast::Source::from_syntax(&tree).unwrap();
        let Expr::Inside { expr, items } = source.modules()[0].assignments()[0].rhs() else {
            panic!()
        };
        let subtraction = |value: &Expr| {
            assert!(
                matches!(
                    value,
                    Expr::Binary {
                        op: BinaryOp::Sub,
                        ..
                    }
                ),
                "{value:?}"
            )
        };
        subtraction(expr);
        let [
            InsideItem::Value(first),
            InsideItem::Range { low, high },
            InsideItem::Value(Expr::Inside { items: nested, .. }),
        ] = items.as_slice()
        else {
            panic!("{items:?}")
        };
        for value in [first, low, high] {
            subtraction(value)
        }
        let [InsideItem::Value(value)] = nested.as_slice() else {
            panic!()
        };
        subtraction(value);
    }
    #[test]
    fn native_membership_items_and_kinds_are_authoritative() {
        for change_class in [false, true] {
            let mut tree = parse_source(
                "module M(input int a,output logic y);assign y=a inside {1,[2:3]};endmodule",
                Path::new("native-inside-shape.sv"),
            )
            .unwrap();
            for expression in &mut tree.declarations.expressions {
                if let celox_sv_syntax::ExprKind::Set(items) = &mut expression.kind {
                    if change_class {
                        items[1].kind = items[0].kind.clone()
                    } else {
                        items.pop();
                    }
                }
            }
            let source = crate::ast::Source::from_syntax(&tree).unwrap();
            let Expr::Inside { items, .. } = source.modules()[0].assignments()[0].rhs() else {
                panic!()
            };
            assert_eq!(items.len(), if change_class { 2 } else { 1 });
            assert!(
                items
                    .iter()
                    .all(|item| matches!(item, InsideItem::Value(_)))
            );
        }
    }
}

#[cfg(test)]
mod function_symbolic;

/// Test fixtures calling private scope kernels must issue a native handle too.
#[cfg(test)]
pub(crate) fn parse_scope_fixture(
    code: &str,
    path: &Path,
) -> Result<ValidatedSyntax, AnalyzerError> {
    let tree = parse_source(code, path)?;
    let handle = tree
        .root_scopes()?
        .first()
        .copied()
        .ok_or_else(|| AnalyzerError::Parse("fixture needs a root scope".into()))?;
    tree.scoped(handle)
}
