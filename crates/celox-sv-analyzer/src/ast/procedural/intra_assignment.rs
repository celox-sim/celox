//! Blocking intra-assignment controls (IEEE 1800-2023 9.4.5, 10.4.1).
use super::*;
use celox_sv_syntax::{ExprId, TimingControlId};

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn intra_assignment(
        &mut self,
        operator: &str,
        nonblocking: bool,
        left: ExprId,
        right: ExprId,
        control: Option<TimingControlId>,
        repeat_count: Option<ExprId>,
    ) -> Converted<Vec<Stmt>> {
        if nonblocking {
            return Err(unsupported("nonblocking intra-assignment timing control"));
        }
        if operator != "=" {
            return Err(unsupported("compound assignment with a timing control"));
        }
        self.allow_timing("intra-assignment timing control")?;
        let control = control.ok_or_else(|| unsupported("intra-assignment cycle control"))?;
        if self.implicit_event_control(control)? {
            return Err(unsupported("implicit intra-assignment event control"));
        }
        let mut output = self.effects(&[right])?;
        let target_effects = self.effects(&[left])?;
        let shape = dimensions::operand_shape(left, self.tree, self.session, &self.dims)?
            .ok_or_else(|| unsupported("intra-assignment target type"))?;
        if type_names::declared_type(&shape)
            .is_some_and(|ty| matches!(ty.kind, TypeKind::Process | TypeKind::Event))
            || shape.real
            || shape.string
            || shape.class.is_some()
            || shape.container.is_some()
            || shape.unpacked_struct.is_some()
            || !shape.unpacked.is_empty()
            || shape.chandle
            || shape.virtual_interface.is_some()
        {
            return Err(unsupported("nonintegral intra-assignment target"));
        }
        let ty = shape
            .pattern_type(&self.tree.functions, &self.dims)
            .ok_or_else(|| unsupported("intra-assignment target type"))?;
        // Bind both sets of effects before binding the assignment, but execute
        // LHS effects only after the wait (10.4.1). RHS conversion to the
        // destination type happens before suspension, just like the table in
        // 9.4.5; the temporary retains width, signedness and four-state bits.
        let Stmt::Assign { lhs, rhs, .. } = self.assignment_value("=", false, (left, right))?
        else {
            return Err(unsupported("complex intra-assignment target"));
        };
        let (_, saved) = self.hidden(ty);
        output.push(Stmt::Assign {
            lhs: LValue::Ident(saved.clone()),
            rhs,
            nonblocking: false,
        });
        let waits = if let Some(stmts) = self.virtual_event_control(control)? {
            stmts
        } else {
            vec![self.native_timing_control(control)?]
        };
        if let Some(count) = repeat_count {
            output.extend(self.effects(&[count])?);
            let count_shape =
                dimensions::operand_shape(count, self.tree, self.session, &self.dims)?
                    .ok_or_else(|| unsupported("intra-assignment repeat count type"))?;
            if count_shape.real
                || count_shape.string
                || count_shape.class.is_some()
                || count_shape.container.is_some()
                || count_shape.unpacked_struct.is_some()
                || !count_shape.unpacked.is_empty()
                || count_shape.chandle
                || count_shape.virtual_interface.is_some()
                || type_names::declared_type(&count_shape)
                    .is_some_and(|ty| matches!(ty.kind, TypeKind::Process | TypeKind::Event))
            {
                return Err(unsupported("nonintegral intra-assignment repeat count"));
            }
            // The existing timed repeat kernel uses a 64-bit counter. Do not
            // silently truncate a wider event count and assign too early.
            if patterns::shape_width(&self.tree.functions, &count_shape, &self.dims)
                .is_none_or(|width| width > 64)
            {
                return Err(unsupported("wide intra-assignment repeat count"));
            }
            output.push(Stmt::Loop {
                kind: LoopKind::Repeat(self.native_value(count)?),
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body: waits,
            });
        } else {
            output.extend(waits);
        }
        output.extend(target_effects);
        output.push(Stmt::Assign {
            lhs,
            rhs: Expr::Ident(saved),
            nonblocking: false,
        });
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnalyzerError, analyze_source};
    use std::path::Path;

    #[test]
    fn keeps_unimplemented_assignment_controls_explicit() {
        for (declarations, statement, diagnostic) in [
            (
                "bit a,b,clk;",
                "a <= #1 b;",
                "nonblocking intra-assignment timing control",
            ),
            (
                "bit a,b,clk;",
                "a <= repeat(2) @(posedge clk) b;",
                "nonblocking intra-assignment timing control",
            ),
            (
                "bit a,b,clk; logic [128:0] n=1;",
                "a = repeat(n) @(posedge clk) b;",
                "wide intra-assignment repeat count",
            ),
            (
                "bit a,b,clk; real n=1.5;",
                "a = repeat(n) @(posedge clk) b;",
                "nonintegral intra-assignment repeat count",
            ),
            (
                "bit a,b,clk; string n=\"x\";",
                "a = repeat(n) @(posedge clk) b;",
                "nonintegral intra-assignment repeat count",
            ),
            ("bit a,b;", "a = ##1 b;", "intra-assignment cycle control"),
            (
                "bit a,b;",
                "a = @* b;",
                "implicit intra-assignment event control",
            ),
            (
                "real a;",
                "a = #1 2.0;",
                "nonintegral intra-assignment target",
            ),
            (
                "string a;",
                "a = #1 \"value\";",
                "nonintegral intra-assignment target",
            ),
            (
                "bit [1:0] a,b;",
                "{a,b} = #1 4'b1010;",
                "complex intra-assignment target",
            ),
        ] {
            let source =
                format!("module Top; {declarations} initial begin {statement} end endmodule");
            let error = analyze_source(&source, Path::new("intra-unsupported.sv")).unwrap_err();
            assert!(
                error.to_string().contains(diagnostic),
                "{statement}: {error}"
            );
        }
    }

    #[test]
    fn controls_still_require_a_process_that_can_suspend() {
        for body in [
            "always_comb a = #1 b;",
            "always_ff @(posedge clk) a = #1 b;",
            "function automatic bit f(); f = #1 b; endfunction initial a=f();",
            "final begin if(0) a = #1 b; end",
        ] {
            let source = format!("module Top; bit clk,a,b; {body} endmodule");
            let error = analyze_source(&source, Path::new("intra-context.sv")).unwrap_err();
            if body.starts_with("final") {
                assert!(
                    matches!(&error, AnalyzerError::Unsupported(text) if text == "timing control in a final procedure"),
                    "{body}: {error}"
                );
            } else {
                assert!(
                    matches!(error, AnalyzerError::TimingControlOutsideProcess { .. }),
                    "{body}: {error}"
                );
            }
        }
    }
}
