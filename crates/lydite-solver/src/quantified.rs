//! Scoped quantified SMT. Deliberately bypasses the quantifier-free structural
//! kernel: its UNSAT rewrites have no quantified-formula correctness contract.
use crate::{Check, solver};
use lydite_ir::{Env, Res, Sort, Term};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
pub enum BinderKind {
    Forall,
    Exists,
}
#[derive(Clone, Debug)]
pub enum QuantifiedFormula {
    Atom(Term),
    And(Vec<Self>),
    Not(Box<Self>),
    Bind {
        kind: BinderKind,
        variables: Vec<Term>,
        body: Box<Self>,
    },
}
impl QuantifiedFormula {
    pub fn negate(self) -> Self {
        Self::Not(Box::new(self))
    }
    pub fn bind(self, kind: BinderKind, variables: Vec<Term>) -> Self {
        if variables.is_empty() {
            self
        } else {
            Self::Bind {
                kind,
                variables,
                body: Box::new(self),
            }
        }
    }
}
fn symbol(term: &Term) -> Res<&str> {
    let name = term
        .0
        .op
        .strip_prefix('@')
        .filter(|_| term.0.args.is_empty())
        .ok_or("quantifier/context must bind a variable")?;
    if name.is_empty()
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        || name.as_bytes()[0].is_ascii_digit()
    {
        return Err("unsafe quantified SMT variable identifier".into());
    }
    Ok(name)
}
fn term_variables(term: &Term, variables: &mut BTreeMap<String, Sort>) -> Res<()> {
    fn visit(
        term: &Term,
        variables: &mut BTreeMap<String, Sort>,
        seen: &mut HashSet<Term>,
    ) -> Res<()> {
        if !seen.insert(term.clone()) {
            return Ok(());
        }
        if term.0.op.starts_with('@') {
            let name = symbol(term)?.to_string();
            if variables
                .insert(name, term.0.sort.clone())
                .is_some_and(|s| s != term.0.sort)
            {
                return Err("quantified variable name has inconsistent sorts".into());
            }
        }
        for arg in &term.0.args {
            visit(arg, variables, seen)?;
        }
        Ok(())
    }
    visit(term, variables, &mut HashSet::new())
}

fn variables(
    formula: &QuantifiedFormula,
    free: &mut BTreeMap<String, Sort>,
    all: &mut BTreeSet<String>,
) -> Res<()> {
    match formula {
        QuantifiedFormula::Atom(term) => term_variables(term, free)?,
        QuantifiedFormula::And(terms) => {
            for term in terms {
                variables(term, free, all)?;
            }
        }
        QuantifiedFormula::Not(term) => variables(term, free, all)?,
        QuantifiedFormula::Bind {
            variables: bound,
            body,
            ..
        } => {
            let mut inside = BTreeMap::new();
            variables(body, &mut inside, all)?;
            let mut names = BTreeSet::new();
            for term in bound {
                let name = symbol(term)?.to_string();
                if !names.insert(name.clone()) {
                    return Err("duplicate variable in SMT binder".into());
                }
                if inside.remove(&name).is_some_and(|s| s != term.0.sort) {
                    return Err("quantifier sort mismatch".into());
                }
                all.insert(name);
            }
            for (name, sort) in inside {
                if free.insert(name, sort.clone()).is_some_and(|s| s != sort) {
                    return Err("free variable sort mismatch".into());
                }
            }
        }
    }
    all.extend(free.keys().cloned());
    Ok(())
}
struct ScopedEmitter {
    reserved: BTreeSet<String>,
    next: usize,
}
impl ScopedEmitter {
    fn term(
        &mut self,
        term: &Term,
        cache: &mut HashMap<Term, String>,
        bindings: &mut Vec<(String, String)>,
    ) -> Res<String> {
        if let Some(name) = cache.get(term) {
            return Ok(name.clone());
        }
        if term.0.op.starts_with('@') {
            return Ok(symbol(term)?.to_string());
        }
        if term.0.args.is_empty() {
            return Ok(term.0.op.clone());
        }
        let args = term
            .0
            .args
            .iter()
            .map(|t| self.term(t, cache, bindings))
            .collect::<Res<Vec<_>>>()?;
        let name = loop {
            let name = format!("quantified_let_{}", self.next);
            self.next += 1;
            if self.reserved.insert(name.clone()) {
                break name;
            }
        };
        bindings.push((name.clone(), format!("({} {})", term.0.op, args.join(" "))));
        cache.insert(term.clone(), name.clone());
        Ok(name)
    }
    fn formula(&mut self, formula: &QuantifiedFormula) -> Res<String> {
        Ok(match formula {
            QuantifiedFormula::Atom(term) => {
                if term.0.sort != Sort::Bool {
                    return Err("quantified formula atom must be Boolean".into());
                }
                let mut bindings = Vec::new();
                let root = self.term(term, &mut HashMap::new(), &mut bindings)?;
                let mut text = String::new();
                for (name, expr) in &bindings {
                    text.push_str(&format!("(let (({name} {expr})) "));
                }
                text.push_str(&root);
                text.push_str(&")".repeat(bindings.len()));
                text
            }
            QuantifiedFormula::And(terms) if terms.is_empty() => "true".into(),
            QuantifiedFormula::And(terms) => format!(
                "(and {})",
                terms
                    .iter()
                    .map(|t| self.formula(t))
                    .collect::<Res<Vec<_>>>()?
                    .join(" ")
            ),
            QuantifiedFormula::Not(term) => format!("(not {})", self.formula(term)?),
            QuantifiedFormula::Bind {
                kind,
                variables,
                body,
            } => {
                let binder = match kind {
                    BinderKind::Forall => "forall",
                    BinderKind::Exists => "exists",
                };
                let declarations = variables
                    .iter()
                    .map(|t| Ok(format!("({} {})", symbol(t)?, t.0.sort.smt())))
                    .collect::<Res<Vec<_>>>()?;
                format!(
                    "({binder} ({}) {})",
                    declarations.join(" "),
                    self.formula(body)?
                )
            }
        })
    }
}
impl Check {
    /// Check an explicit quantified formula with independent scoped SMT emission.
    /// Context contains only free variables. A fresh SAT recheck obtains models
    /// only when such a concrete witness exists; closed truth claims have none.
    pub fn query_quantified(
        &mut self,
        name: &str,
        formula: &QuantifiedFormula,
        expect_sat: bool,
        context: &Env,
        timeout_ms: u64,
    ) -> Res<()> {
        let start = Instant::now();
        let mut free = BTreeMap::new();
        let mut all = BTreeSet::new();
        variables(formula, &mut free, &mut all)?;
        let mut ctx = BTreeMap::new();
        for (label, term) in context {
            let symbol = symbol(term)?;
            if let Some(sort) = free.get(symbol) {
                if sort != &term.0.sort {
                    return Err(format!("context {label} has incorrect sort"));
                }
            } else if all.contains(symbol) {
                return Err(format!("context {label} is bound in quantified query"));
            } else {
                free.insert(symbol.to_string(), term.0.sort.clone());
                all.insert(symbol.to_string());
            }
            ctx.insert(label, symbol);
        }
        let expression = ScopedEmitter {
            reserved: all,
            next: 0,
        }
        .formula(formula)?;
        let declarations = free
            .iter()
            .map(|(name, sort)| format!("(declare-fun {name} () {})", sort.smt()))
            .collect::<Vec<_>>()
            .join("\n");
        let mut script = format!(
            "(set-option :timeout {timeout_ms})\n(set-option :produce-models true)\n(set-logic ALL)\n{declarations}\n(assert {expression})\n(check-sat)\n"
        );
        let emission_seconds = start.elapsed().as_secs_f64();
        if crate::z3::finite_only() {
            let (search_hint, search_hint_source) =
                crate::z3::resolve_finite_search_hint(expect_sat, None)?;
            let reason =
                "quantified queries are outside the finite Bool/BV backend; Z3 not invoked";
            fs::write(self.out.join(format!("{name}.smt2")), &script).map_err(|e| e.to_string())?;
            fs::write(
                self.out.join(format!("{name}.out")),
                format!("unknown\n; {reason}\n"),
            )
            .map_err(|e| e.to_string())?;
            self.reports.push(json!({"name":name,"status":"unknown","solver_result":"unknown","backend":"finite_bv",
                "seconds":start.elapsed().as_secs_f64(),"emission_seconds":emission_seconds,"z3_seconds":0.0,
                "logical_expectation":if expect_sat { "sat" } else { "unsat" },
                "search_hint_source":search_hint_source,
                "finite":{"reason":reason,"original_formula_validated":false,
                    "search_hint":search_hint.as_str(),"search_strategy":"unsupported_quantified"},
                "kernel":{"enabled":false,"reason":"quantified formulas bypass the quantifier-free structural kernel"},
                "context_symbols":ctx,"concrete_model":false,"evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
            return Ok(());
        }
        let z3_start = Instant::now();
        let mut raw = solver(&self.z3, &script)?;
        let verdict = raw.trim().to_string();
        let status = match verdict.as_str() {
            "sat" if expect_sat => "passed",
            "sat" => "counterexample",
            "unsat" if expect_sat => "failed",
            "unsat" => "passed",
            "unknown" => "unknown",
            _ => return Err(format!("unexpected quantified Z3 output {raw}")),
        };
        let mut model = false;
        if verdict == "sat" && !ctx.is_empty() {
            script.push_str("(get-model)\n");
            script.push_str(&format!(
                "(get-value ({}))\n",
                ctx.values().copied().collect::<Vec<_>>().join(" ")
            ));
            raw = solver(&self.z3, &script)?;
            if raw.lines().next().map(str::trim) != Some("sat") {
                return Err(
                    "quantified SAT model recheck did not return sat; no SAT result certified"
                        .into(),
                );
            }
            model = true;
        }
        fs::write(self.out.join(format!("{name}.smt2")), script).map_err(|e| e.to_string())?;
        fs::write(self.out.join(format!("{name}.out")), raw).map_err(|e| e.to_string())?;
        self.reports.push(json!({"name":name,"status":status,"solver_result":verdict,"backend":"z3_quantified","seconds":start.elapsed().as_secs_f64(),"emission_seconds":emission_seconds,"z3_seconds":z3_start.elapsed().as_secs_f64(),"timeout_ms":timeout_ms,"kernel":{"enabled":false,"reason":"quantified formulas bypass the quantifier-free structural kernel"},"context_symbols":ctx,"concrete_model":model,"evidence":format!("{name}.smt2"),"solver_output":format!("{name}.out")}));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lydite_ir::{eq, not, var};
    #[test]
    fn local_let_names_cannot_capture_quantifier_variables() {
        let variable = var("quantified_let_0".into(), Sort::Bool);
        let body = QuantifiedFormula::Atom(eq(variable.clone(), not(variable.clone())))
            .bind(BinderKind::Forall, vec![variable]);
        let mut free = BTreeMap::new();
        let mut all = BTreeSet::new();
        variables(&body, &mut free, &mut all).unwrap();
        assert!(free.is_empty());
        let emitted = ScopedEmitter {
            reserved: all,
            next: 0,
        }
        .formula(&body)
        .unwrap();
        assert!(emitted.starts_with("(forall ((quantified_let_0 Bool))"));
        assert!(!emitted.contains("(let ((quantified_let_0 "));
        assert!(emitted.contains("(let ((quantified_let_1 (not quantified_let_0)))"));
    }
    #[test]
    fn bound_variables_never_escape_to_global_declarations() {
        let variable = var("bound_only".into(), Sort::Bool);
        let formula =
            QuantifiedFormula::Atom(variable.clone()).bind(BinderKind::Exists, vec![variable]);
        let mut free = BTreeMap::new();
        let mut all = BTreeSet::new();
        variables(&formula, &mut free, &mut all).unwrap();
        assert!(free.is_empty());
        assert!(all.contains("bound_only"));
    }
}
