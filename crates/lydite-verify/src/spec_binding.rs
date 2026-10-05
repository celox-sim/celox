//! Sound, deliberately restricted implementation bridge: a supplied state-only
//! deterministic witness into every private state and shared observation.
use lydite_ir::*;
use lydite_solver::Check;
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn pairs(from: &Env, to: &Env) -> BTreeMap<Term, Term> {
    from.iter()
        .map(|(name, t)| (t.clone(), to[name].clone()))
        .collect()
}
fn mapped(values: &Env, subst: &BTreeMap<Term, Term>) -> Env {
    values
        .iter()
        .map(|(name, t)| (name.clone(), substitute(t, subst)))
        .collect()
}
fn both(a: Term, b: Term) -> Term {
    and(a, b)
}
fn disjoin(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}
/// Original safety predicates shared by induction and bounded reset replay.
pub(crate) struct SafetyModel {
    pub reset: Term,
    pub reset_good: Term,
    pub invariant: Term,
    pub context: Env,
    pub checks: Vec<(String, Term, Option<String>)>,
}
pub(crate) fn safety_model(spec: &Specification) -> Res<SafetyModel> {
    let implementation = spec.implementation().ok_or("missing implementation")?;
    let product = &spec.compositions()[&implementation.composition];
    let machine = &implementation.machine;
    let reset_sub = pairs(&machine.state, &machine.reset);
    let next_sub = pairs(&machine.state, &machine.next);
    let reset_observations = mapped(&implementation.observations, &reset_sub);
    let next_observations = mapped(&implementation.observations, &next_sub);
    let mut initial = boolv(true);
    let mut reset_invariant = boolv(true);
    let mut invariant = boolv(true);
    let mut next_invariant = boolv(true);
    let mut relations = spec
        .operations()
        .iter()
        .map(|op| (op.clone(), boolv(true)))
        .collect::<BTreeMap<_, _>>();
    let mut stutter = boolv(true);
    let mut context = Env::new();
    for (name, t) in &machine.state {
        context.insert(format!("impl.{name}"), t.clone());
        context.insert(format!("impl_next.{name}"), machine.next[name].clone());
    }
    for (name, t) in spec.inputs() {
        context.insert(format!("i.{name}"), t.clone());
    }
    for (name, t) in &implementation.observations {
        stutter = both(stutter, eq(t.clone(), next_observations[name].clone()));
        context.insert(format!("o.{name}"), t.clone());
        context.insert(format!("no.{name}"), next_observations[name].clone());
    }
    for name in &product.members {
        let c = &spec.components()[name];
        let current = &implementation.states[name];
        let next = mapped(current, &next_sub);
        let reset = mapped(current, &reset_sub);
        let mut subst = pairs(&c.state, current);
        subst.extend(pairs(spec.observations(), &implementation.observations));
        invariant = both(invariant, substitute(&c.invariant, &subst));
        subst.extend(pairs(&c.next_state, &next));
        subst.extend(pairs(spec.next_observations(), &next_observations));
        for op in spec.operations() {
            relations.insert(
                op.clone(),
                both(relations[op].clone(), substitute(&c.steps[op], &subst)),
            );
        }
        let mut subst = pairs(&c.state, &reset);
        subst.extend(pairs(spec.observations(), &reset_observations));
        initial = both(initial, substitute(&c.initial, &subst));
        reset_invariant = both(reset_invariant, substitute(&c.invariant, &subst));
        let mut subst = pairs(&c.state, &next);
        subst.extend(pairs(spec.observations(), &next_observations));
        next_invariant = both(next_invariant, substitute(&c.invariant, &subst));
        for (field, t) in current {
            stutter = both(stutter, eq(t.clone(), next[field].clone()));
            context.insert(format!("{name}.{field}"), t.clone());
            context.insert(format!("{name}_next.{field}"), next[field].clone());
        }
    }
    let reset = spec.inputs()[&implementation.reset_input].clone();
    let reset_good = both(initial, reset_invariant);
    let active = both(not(reset.clone()), invariant.clone());
    let mut checks = vec![];
    let selectors = implementation
        .operations
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut overlap = boolv(false);
    for (index, a) in selectors.iter().enumerate() {
        for b in selectors.iter().skip(index + 1) {
            overlap = disjoin(overlap, both(a.clone(), b.clone()));
        }
    }
    checks.push((
        "binding_operation_exclusive".into(),
        both(active.clone(), overlap),
        None,
    ));
    for (index, (operation, selector)) in implementation.operations.iter().enumerate() {
        let good = both(relations[operation].clone(), next_invariant.clone());
        checks.push((
            format!("binding_operation_{index}"),
            both(active.clone(), both(selector.clone(), not(good))),
            Some(operation.clone()),
        ));
    }
    let any = selectors.into_iter().fold(boolv(false), disjoin);
    checks.push((
        "binding_no_operation_stutters".into(),
        both(active, both(not(any), not(stutter))),
        None,
    ));
    Ok(SafetyModel {
        reset,
        reset_good,
        invariant,
        context,
        checks,
    })
}
pub(crate) fn obligations(spec: &Specification, q: &mut Check) -> Res<Option<Value>> {
    let Some(implementation) = spec.implementation() else {
        return Ok(None);
    };
    let before = q.reports.len();
    let product = &spec.compositions()[&implementation.composition];
    let SafetyModel {
        reset,
        reset_good,
        invariant,
        context,
        checks,
    } = safety_model(spec)?;
    q.query(
        "binding_reset_nonempty",
        both(reset.clone(), reset_good.clone()),
        true,
        &context,
    )?;
    q.query(
        "binding_reset_establishes_product",
        both(reset.clone(), not(reset_good)),
        false,
        &context,
    )?;
    let mut response_results = crate::progress::responses(
        implementation,
        reset.clone(),
        invariant.clone(),
        &context,
        spec.inputs(),
        q,
    )?;
    for (name, bad, operation) in checks {
        q.query(&name, bad, false, &context)?;
        if let Some(operation) = operation {
            q.reports.last_mut().unwrap()["operation"] = json!(operation);
        }
    }
    let progress_limitation = if implementation.responses.is_empty() {
        "No liveness, fairness, progress, deadlock freedom, or implementation total-correctness claim"
    } else {
        "Only explicitly declared responses have conditional bounded progress; no general fairness, deadlock freedom or total-correctness claim"
    };
    let reports = q.reports[before..].to_vec();
    let status = if reports
        .iter()
        .any(|r| r["status"] == "counterexample" || r["status"] == "failed_nonvacuity")
    {
        "failed"
    } else if reports.iter().any(|r| r["status"] == "unknown") {
        "unknown"
    } else {
        "verified"
    };
    for response in &mut response_results {
        response["requires_verified_binding"] = json!(true);
        if response["status"] == "verified" && status != "verified" {
            response["status"] = json!(if status == "unknown" {
                "unknown"
            } else {
                "not_established_due_to_binding_failure"
            });
        }
    }
    Ok(Some(
        json!({"responses":response_results,"status":status,"composition":implementation.composition,"members":product.members,"obligations":reports,"claim":"From reset, the supplied state-only abstraction maps each selected implementation step to a synchronized relational product step; no-operation steps stutter in all mapped state and observations","limitations":["This is a deterministic witness into the relational specification, not equality of behavior sets or abstraction surjectivity",progress_limitation,"Every mapped-invariant implementation state is checked, including unreachable states; reset has priority and restarts the abstract initial state","Operation selectors must be pairwise exclusive on nonreset mapped-invariant states; no selector means explicit abstract stutter"]}),
    ))
}
