//! Finite complete-execution quantification, separate from legacy admission.
use hwverify_ir::*;
use hwverify_solver::{BinderKind, Check, QuantifiedFormula as Formula};
use serde_json::{Value, json};

fn input_prefix(mut body: Formula, inputs: &[InputQuantifier]) -> Formula {
    for binder in inputs.iter().rev() {
        let kind = match binder.kind {
            InputQuantifierKind::Forall => BinderKind::Forall,
            InputQuantifierKind::Exists => BinderKind::Exists,
        };
        body = body.bind(kind, binder.variables.values().cloned().collect());
    }
    body
}
fn truth(evidence: &Value) -> Option<bool> {
    match evidence["solver_result"].as_str() {
        Some("sat") => Some(true),
        Some("unsat") => Some(false),
        _ => None,
    }
}
fn run(query: &mut Check, name: &str, formula: &Formula) -> Res<Value> {
    query.query_quantified(name, formula, true, &Env::new(), 10_000)?;
    Ok(query.reports.last().unwrap().clone())
}

pub(crate) fn check_example(
    spec: &Specification,
    members: &[String],
    example: &TraceExample,
    id: &str,
    query: &mut Check,
) -> Res<Value> {
    let quantification = example
        .quantification
        .as_ref()
        .ok_or("missing quantification")?;
    let (relation, expected, execution_context) =
        crate::specification::trace_relations(spec, members, example)?;
    let execution: Vec<_> = execution_context.values().cloned().collect();
    let feasible = Formula::Atom(relation.clone()).bind(BinderKind::Exists, execution.clone());
    let good = Formula::Atom(and(relation.clone(), expected.clone()))
        .bind(BinderKind::Exists, execution.clone());
    let every = Formula::Atom(node(
        Sort::Bool,
        "=>",
        vec![relation.clone(), expected.clone()],
    ))
    .bind(BinderKind::Forall, execution.clone());
    let body = match quantification.execution {
        ExecutionQuantifier::Exists => good.clone(),
        ExecutionQuantifier::NotExists => good.clone().negate(),
        ExecutionQuantifier::Forall => Formula::And(vec![feasible.clone(), every]),
    };
    let evidence = run(
        query,
        id,
        &input_prefix(body.clone(), &quantification.inputs),
    )?;
    let valid = truth(&evidence);
    let feasibility = run(
        query,
        &format!("{id}_feasibility"),
        &input_prefix(feasible.clone(), &quantification.inputs),
    )?;
    let nonvacuous = if quantification.execution == ExecutionQuantifier::NotExists {
        run(
            query,
            &format!("{id}_nonvacuous"),
            &input_prefix(
                Formula::And(vec![feasible.clone(), body.clone()]),
                &quantification.inputs,
            ),
        )?
    } else {
        evidence.clone()
    };
    let nonvacuity = truth(&nonvacuous);
    let status = match valid {
        Some(true) => "passed",
        Some(false) => "failed",
        None => "unknown",
    };
    let vacuity = match (quantification.execution, valid, nonvacuity) {
        (_, None, _) => "unknown",
        (_, Some(false), _) => "claim_false",
        (ExecutionQuantifier::NotExists, Some(true), Some(false)) => "vacuous_exclusion",
        (ExecutionQuantifier::NotExists, Some(true), None) => "exclusion_nonvacuity_unknown",
        _ => "nonvacuous",
    };
    // A quantified strategy need not have a single finite witness. Extract only
    // concrete existential witnesses or concrete all-universal counterexamples.
    let all_existential = quantification
        .inputs
        .iter()
        .all(|b| b.kind == InputQuantifierKind::Exists);
    let all_universal = quantification
        .inputs
        .iter()
        .all(|b| b.kind == InputQuantifierKind::Forall);
    let sample = match (quantification.execution, valid) {
        (ExecutionQuantifier::Exists, Some(true)) if all_existential => Some((
            and(relation.clone(), expected.clone()),
            "satisfying_execution",
        )),
        (ExecutionQuantifier::NotExists, Some(false)) if all_universal => Some((
            and(relation.clone(), expected.clone()),
            "excluded_execution_counterexample",
        )),
        (ExecutionQuantifier::Forall, Some(false)) if all_universal => Some((
            and(relation.clone(), not(expected)),
            "postcondition_counterexample",
        )),
        _ => None,
    };
    let mut concrete = Value::Null;
    let mut witness_diagnostics = Vec::new();
    let mut postcondition_counterexample_found = None;
    if let Some((term, purpose)) = sample {
        let mut context = execution_context;
        for binder in &quantification.inputs {
            context.extend(
                binder
                    .variables
                    .iter()
                    .map(|(name, term)| (format!("q.{name}"), term.clone())),
            );
        }
        query.query_quantified(
            &format!("{id}_witness"),
            &Formula::Atom(term),
            true,
            &context,
            10_000,
        )?;
        let diagnostic = query.reports.last().unwrap();
        postcondition_counterexample_found = truth(diagnostic);
        let attempt = json!({"purpose":purpose,"found":truth(diagnostic),"evidence":diagnostic});
        if diagnostic["concrete_model"] == true {
            concrete = attempt.clone();
        }
        witness_diagnostics.push(attempt);
    }
    // If no complete bad execution exists, universal failure can instead be
    // witnessed by an input choice with no feasible complete execution. Failure
    // of all-input existential admission likewise has a counterexample input.
    let input_diagnostic = if all_universal && valid == Some(false) {
        match quantification.execution {
            ExecutionQuantifier::Exists => {
                Some((good.negate(), "input_without_satisfying_execution"))
            }
            ExecutionQuantifier::Forall if postcondition_counterexample_found == Some(false) => {
                Some((feasible.negate(), "input_without_feasible_execution"))
            }
            _ => None,
        }
    } else if all_existential
        && valid == Some(true)
        && !quantification.inputs.is_empty()
        && quantification.execution != ExecutionQuantifier::Exists
    {
        Some((body, "satisfying_input_choice"))
    } else {
        None
    };
    if let Some((formula, purpose)) = input_diagnostic {
        let context: Env = quantification
            .inputs
            .iter()
            .flat_map(|binder| {
                binder
                    .variables
                    .iter()
                    .map(|(name, term)| (format!("q.{name}"), term.clone()))
            })
            .collect();
        query.query_quantified(
            &format!("{id}_input_witness"),
            &formula,
            true,
            &context,
            10_000,
        )?;
        let diagnostic = query.reports.last().unwrap();
        let attempt = json!({"purpose":purpose,"found":truth(diagnostic),"evidence":diagnostic});
        if diagnostic["concrete_model"] == true {
            concrete = attempt.clone();
        }
        witness_diagnostics.push(attempt);
    }
    let input_binders: Vec<_> = quantification.inputs.iter().map(|binder| json!({
        "kind":match binder.kind { InputQuantifierKind::Forall => "forall", InputQuantifierKind::Exists => "exists" },
        "variables":binder.variables.iter().map(|(name, term)| (name.clone(), term.0.sort.smt())).collect::<std::collections::BTreeMap<_, _>>()
    })).collect();
    Ok(json!({
        "expect":quantification.execution.name(),"steps":example.trace.len(),"status":status,"valid":valid,
        "quantification":{"input_prefix":input_binders,"execution":quantification.execution.name(),"execution_position":"innermost","execution_scope":"complete finite trace: private state and every unspecified input/output; initial conditions are assumptions; all expected observations and ensure predicates form the tested postcondition"},
        "feasibility":{"holds":truth(&feasibility),"scope":"the same ordered input prefix, applied to existence of a complete trace independently of expected results","evidence":feasibility},
        "nonvacuous":{"holds":nonvacuity,"scope":"feasibility and the execution claim share the same outer input choices","evidence":nonvacuous},
        "vacuity":vacuity,"evidence":evidence,"concrete_witness":concrete,"witness_diagnostics":witness_diagnostics,
        "witness_note":"Closed quantified truth claims and alternating input strategies do not generally have a single concrete witness; a separate SAT diagnostic, when available, records a fresh model",
        "claim":"Only complete executions of this supplied finite trace are quantified; no liveness, arbitrary extension, or total-correctness claim"
    }))
}
