//! Bounded bit coverage for static results and scratch locals filled by constant loops.
use super::*;
use num_bigint::BigUint;

const MAX_BITS: usize = 65536;
const MAX_STEPS: usize = 65536;

pub(super) fn proves(
    state: &AnalysisState,
    subroutine: &Subroutine,
    constants: &HashMap<String, i128>,
    locals: &[LocalVariable],
) -> bool {
    let Some(name) = subroutine.return_var.as_deref() else {
        return false;
    };
    let Some(width) = subroutine
        .return_type
        .as_ref()
        .and_then(|ty| ty.resolved_width())
        .filter(|width| (1..=MAX_BITS).contains(width))
    else {
        return false;
    };
    proves_variable(state, name, width, &subroutine.body, constants, locals)
}

pub(super) fn proves_local(
    state: &AnalysisState,
    name: &str,
    body: &[Stmt],
    constants: &HashMap<String, i128>,
    locals: &[LocalVariable],
) -> bool {
    let Some(local) = locals.iter().find(|local| local.name == name) else {
        return false;
    };
    // The lowered local type's width already includes unpacked dimensions.
    let Some(width) = local.r#type.resolved_width() else {
        return false;
    };
    proves_variable(state, name, width, body, constants, locals)
}

pub(super) fn proves_variable(
    state: &AnalysisState,
    name: &str,
    width: usize,
    body: &[Stmt],
    constants: &HashMap<String, i128>,
    locals: &[LocalVariable],
) -> bool {
    if !(1..=MAX_BITS).contains(&width) {
        return false;
    }
    let mut env = constants.clone();
    let mut types = HashMap::default();
    for local in locals {
        env.remove(&local.name);
        if local.r#type.unpacked_ranges().is_empty()
            && let Some(width) = local
                .r#type
                .resolved_width()
                .filter(|width| (1..=64).contains(width))
        {
            let signed = local.r#type.is_signed();
            types.insert(local.name.clone(), (width, signed));
            env.insert(
                dimensions::parameter_width_marker(&local.name),
                width as i128,
            );
            env.insert(
                dimensions::parameter_signed_marker(&local.name),
                i128::from(signed),
            );
        }
    }
    let mut proof = Proof {
        state,
        name,
        width,
        written: BigUint::default(),
        full: (BigUint::from(1u8) << width) - BigUint::from(1u8),
        env,
        types,
        remaining: MAX_STEPS,
    };
    proof.block(body) == Some(true)
}

struct Proof<'a> {
    state: &'a AnalysisState,
    name: &'a str,
    width: usize,
    written: BigUint,
    full: BigUint,
    env: HashMap<String, i128>,
    types: HashMap<String, (usize, bool)>,
    remaining: usize,
}

impl Proof<'_> {
    fn readable(&self, expression: &Expr) -> bool {
        pure(expression) && (self.written == self.full || !expr_reads(expression, self.name))
    }

    fn value(&self, expression: &Expr) -> Option<i128> {
        if !self.readable(expression) {
            return None;
        }
        let dims = PackedDimensions::new(HashMap::default(), &self.env, &HashMap::default());
        let value = expr_to_index_const(self.state, expression.clone(), &dims)?;
        eval_ast_const_expr(self.state, &value, &self.env)
    }

    fn store(&mut self, name: &str, value: Option<i128>) -> Option<()> {
        self.env.remove(name);
        if let Some(value) = value {
            let (width, signed) = *self.types.get(name)?;
            let low = if signed { -(1i128 << (width - 1)) } else { 0 };
            let high = if signed {
                (1i128 << (width - 1)) - 1
            } else {
                (1i128 << width) - 1
            };
            // Refuse to prove through assignment truncation. This keeps the
            // abstract loop's iteration sequence equal to the declared type's.
            if !(low..=high).contains(&value) {
                return None;
            }
            self.env.insert(name.to_string(), value);
        }
        Some(())
    }

    /// `true` means no entry bit can subsequently be observed. Unknown control
    /// flow or calls give no proof; the ordinary lifetime check requires persistent storage.
    fn block(&mut self, statements: &[Stmt]) -> Option<bool> {
        for statement in statements {
            self.remaining = self.remaining.checked_sub(1)?;
            match statement {
                Stmt::Local { name, init } if name != self.name => {
                    let value = init.as_ref().and_then(|value| self.value(value));
                    if let Some(init) = init {
                        if !self.readable(init) {
                            return None;
                        }
                    }
                    self.store(name, value)?;
                }
                Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking: false,
                } => {
                    if !self.readable(rhs) {
                        return None;
                    }
                    if lhs.name() == self.name {
                        match lhs {
                            LValue::Ident(_) => self.written = self.full.clone(),
                            LValue::Select { msb, lsb, .. } => {
                                if !pure_constant(msb) || !pure_constant(lsb) {
                                    return None;
                                }
                                let left = eval_ast_const_expr(self.state, msb, &self.env)?;
                                let right = eval_ast_const_expr(self.state, lsb, &self.env)?;
                                let low = usize::try_from(left.min(right)).ok()?;
                                let high = usize::try_from(left.max(right)).ok()?;
                                if high >= self.width {
                                    return None;
                                }
                                for bit in low..=high {
                                    self.written.set_bit(bit as u64, true);
                                }
                            }
                        }
                    } else if let LValue::Ident(name) = lhs {
                        let value = self.value(rhs);
                        self.store(name, value)?;
                    } else if let LValue::Select { name, msb, lsb, .. } = lhs {
                        // A partial store loses our numeric value for this
                        // other local, but does not read its previous bits.
                        if !pure_constant(msb)
                            || !pure_constant(lsb)
                            || const_expr_reads(msb, self.name)
                            || const_expr_reads(lsb, self.name)
                        {
                            return None;
                        }
                        self.env.remove(name);
                    }
                }
                Stmt::Loop {
                    kind: crate::procedural::LoopKind::For,
                    init,
                    condition: Some(condition),
                    step,
                    body,
                } => {
                    if self.block(init)? {
                        return Some(true);
                    }
                    while self.value(condition)? != 0 {
                        self.remaining = self.remaining.checked_sub(1)?;
                        if self.block(body)? || self.block(step)? {
                            return Some(true);
                        }
                    }
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    let branch = if self.value(condition)? != 0 {
                        then_body
                    } else {
                        else_body
                    };
                    if self.block(branch)? {
                        return Some(true);
                    }
                }
                Stmt::Return(Some(value)) if self.readable(value) => return Some(true),
                _ => return None,
            }
            if self.written == self.full {
                return Some(true);
            }
        }
        Some(self.written == self.full)
    }
}

fn pure_constant(expression: &ConstExpr) -> bool {
    match expression {
        ConstExpr::Ident(_) | ConstExpr::Literal(_) => true,
        ConstExpr::Function { .. } => false,
        ConstExpr::Select { expr, bit } => pure_constant(expr) && pure_constant(bit),
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            pure_constant(expr) && pure_constant(msb) && pure_constant(lsb)
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => pure_constant(expr),
        ConstExpr::Binary { left, right, .. } => pure_constant(left) && pure_constant(right),
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => pure_constant(condition) && pure_constant(then_expr) && pure_constant(else_expr),
    }
}

fn pure(expression: &Expr) -> bool {
    match expression {
        Expr::Ident(_) | Expr::Literal(_) => true,
        Expr::Call { .. } => false,
        Expr::Select { expr, msb, lsb, .. } => {
            pure(expr) && pure_constant(msb) && pure_constant(lsb)
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => pure(expr),
        Expr::Binary { left, right, .. } => pure(left) && pure(right),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => pure(condition) && pure(then_expr) && pure(else_expr),
        Expr::Concat(parts) => parts.iter().all(pure),
        Expr::RepeatConcat { count, parts } => pure_constant(count) && parts.iter().all(pure),
        Expr::Inside { expr, items } => {
            pure(expr) && items.iter().all(|item| item.exprs().into_iter().all(pure))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn incomplete_or_entry_dependent_scratch_arrays_retain_storage() {
        for body in [
            "for(integer i=0;i<1;i=i+1) words[i]=a;",
            "for(integer i=0;i<2;i=i+1) words[0]=a;",
            "for(integer i=0;i<2;i=i+1) words[i]=words[i]^a;",
            "if(a[0]) for(integer i=0;i<2;i=i+1) words[i]=a;",
            "other[words[0]]=0; for(integer i=0;i<2;i=i+1) words[i]=a;",
        ] {
            let source = format!(
                "module Top(input logic[7:0] a, output logic[7:0] y); \
                function [7:0] f(input logic[7:0] a); \
                logic[7:0] words[1:0]; logic[7:0] other; {body} \
                f=words[0]+words[1]; endfunction assign y=f(a); endmodule"
            );
            assert_retained(&source, "words");
        }
    }

    #[test]
    fn incomplete_or_entry_dependent_results_retain_storage() {
        for body in [
            "for(integer i=0;i<3;i=i+1) f[i]=a[i];",
            "for(integer i=0;i<4;i=i+1) f[0]=a[i];",
            "for(integer i=0;i<0;i=i+1) f[i]=a[i];",
            "if(a[0]) for(integer i=0;i<4;i=i+1) f[i]=a[i];",
            "for(integer i=0;i<4;i=i+1) begin if(i==2) break; f[i]=a[i]; end",
            "for(integer i=0;i<4;i=i+1) f[i]=f[i]^a[i];",
            "for(byte i=127;i<129;i=i+1) f[i%4]=a[i%4];",
        ] {
            let source = format!(
                "module Top(input logic[3:0] a, output logic[3:0] y); \
                function [3:0] f(input logic[3:0] a); {body} endfunction \
                assign y=f(a); endmodule"
            );
            assert_retained(&source, "f");
        }
    }

    #[test]
    fn index_calls_do_not_prove_bit_coverage() {
        let source = r#"
            module Top(input logic[3:0] a, output logic[3:0] y);
                function automatic integer mutate(inout integer index);
                    index=3; return 0;
                endfunction
                function [3:0] f(input logic[3:0] a);
                    for(integer i=0;i<4;i=i+1) f[mutate(i)]=a[i];
                endfunction
                assign y=f(a);
            endmodule
        "#;
        assert_retained(source, "f");
    }

    fn assert_retained(source: &str, variable: &str) {
        let ir =
            crate::analyze_source(source, std::path::Path::new("retained-storage.sv")).unwrap();
        let module = &ir.modules()[0];
        let function = module
            .subroutines()
            .iter()
            .find(|function| function.name == "f")
            .unwrap();
        let prefix = format!("{variable}@");
        let name = function
            .persistent_locals
            .iter()
            .find(|name| name.starts_with(&prefix))
            .unwrap_or_else(|| panic!("missing persistent {variable}: {source}"));
        assert!(module.signals().iter().any(|signal| signal.name() == name));
        assert!(!module.locals().iter().any(|local| &local.name == name));
    }
}
