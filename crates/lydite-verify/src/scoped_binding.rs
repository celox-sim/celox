//! Symbolic independent-action implementation bridge for scoped specifications.
//!
//! The trace IR already contains the conjunction of each activated local
//! relation and equality for each inactive leaf's private state. Replacing its
//! Boolean action controls with typed implementation selectors checks all action
//! combinations at once, without constructing the power set of exported actions.
use lydite_ir::*;
use lydite_solver::Check;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn pairs(from: &Env, to: &Env) -> BTreeMap<Term, Term> {
    from.iter()
        .map(|(name, term)| (term.clone(), to[name].clone()))
        .collect()
}

fn mapped(values: &Env, substitutions: &BTreeMap<Term, Term>) -> Env {
    values
        .iter()
        .map(|(name, term)| (name.clone(), substitute(term, substitutions)))
        .collect()
}

fn disjoin(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}

pub(crate) fn obligations(target: &ScopedTarget, q: &mut Check) -> Res<Option<Value>> {
    let Some(implementation) = &target.implementation else {
        return Ok(None);
    };
    let before = q.reports.len();
    let spec = &target.specification;
    let product = &spec.compositions()[&implementation.composition];
    let tick = spec.operations().iter().next().unwrap();
    let machine = &implementation.machine;
    let reset_sub = pairs(&machine.state, &machine.reset);
    let next_sub = pairs(&machine.state, &machine.next);
    let reset_observations = mapped(&implementation.observations, &reset_sub);
    let next_observations = mapped(&implementation.observations, &next_sub);
    let mut initial = boolv(true);
    let mut reset_invariant = boolv(true);
    let mut invariant = boolv(true);
    let mut next_invariant = boolv(true);
    let mut relation = boolv(true);
    let mut context = Env::new();
    let mut input_sub = BTreeMap::new();
    for (name, term) in &target.implementation_inputs {
        context.insert(format!("i.{name}"), term.clone());
        if let Some(input) = spec.inputs().get(name) {
            input_sub.insert(input.clone(), term.clone());
        }
    }
    for (action, input) in &target.action_inputs {
        input_sub.insert(
            spec.inputs()[input].clone(),
            implementation.operations[action].clone(),
        );
        context.insert(
            format!("action.{action}"),
            implementation.operations[action].clone(),
        );
    }
    for (name, term) in &machine.state {
        context.insert(format!("impl.{name}"), term.clone());
        context.insert(format!("impl_next.{name}"), machine.next[name].clone());
    }
    for (name, term) in &implementation.observations {
        context.insert(format!("o.{name}"), term.clone());
        context.insert(format!("no.{name}"), next_observations[name].clone());
    }
    for name in &product.members {
        let component = &spec.components()[name];
        let current = &implementation.states[name];
        let next = mapped(current, &next_sub);
        let reset = mapped(current, &reset_sub);
        let mut subst = pairs(&component.state, current);
        subst.extend(pairs(spec.observations(), &implementation.observations));
        invariant = and(invariant, substitute(&component.invariant, &subst));
        subst.extend(pairs(&component.next_state, &next));
        subst.extend(pairs(spec.next_observations(), &next_observations));
        subst.extend(input_sub.clone());
        relation = and(relation, substitute(&component.steps[tick], &subst));

        let mut subst = pairs(&component.state, &reset);
        subst.extend(pairs(spec.observations(), &reset_observations));
        initial = and(initial, substitute(&component.initial, &subst));
        reset_invariant = and(reset_invariant, substitute(&component.invariant, &subst));
        let mut subst = pairs(&component.state, &next);
        subst.extend(pairs(spec.observations(), &next_observations));
        next_invariant = and(next_invariant, substitute(&component.invariant, &subst));
        for (field, term) in current {
            // These generated identities are rewritten to explicit private.*
            // namespaces by the scoped report presenter, never alias namespaces.
            context.insert(format!("{name}.{field}"), term.clone());
            context.insert(format!("{name}_next.{field}"), next[field].clone());
        }
    }

    let reset = target.implementation_inputs[&implementation.reset_input].clone();
    let reset_good = and(initial, reset_invariant);
    q.query(
        "binding_reset_nonempty",
        and(reset.clone(), reset_good.clone()),
        true,
        &context,
    )?;
    q.query(
        "binding_reset_establishes_product",
        and(reset.clone(), not(reset_good)),
        false,
        &context,
    )?;
    let mut response_results = crate::progress::responses(
        implementation,
        reset.clone(),
        invariant.clone(),
        &context,
        &target.implementation_inputs,
        q,
    )?;
    let active = and(not(reset), invariant);

    // A local operation is activated once, even when several exported groups
    // reach it. Only distinct operations of one private instance can conflict.
    // Importantly, exclusivity is an obligation, not an assumption added to the
    // preservation query: overlapping selectors cannot verify vacuously.
    for (index, name) in product.members.iter().enumerate() {
        let operations = &target.leaf_operations[name];
        if operations.len() < 2 {
            continue;
        }
        let activations = operations
            .values()
            .map(|actions| {
                actions.iter().fold(boolv(false), |acc, action| {
                    disjoin(acc, implementation.operations[action].clone())
                })
            })
            .collect::<Vec<_>>();
        let mut overlap = boolv(false);
        for (i, a) in activations.iter().enumerate() {
            for b in activations.iter().skip(i + 1) {
                overlap = disjoin(overlap, and(a.clone(), b.clone()));
            }
        }
        q.query(
            &format!("binding_leaf_exclusive_{index}"),
            and(active.clone(), overlap),
            false,
            &context,
        )?;
        let report = q.reports.last_mut().unwrap();
        report["instance"] = json!(target.instance_paths[name]);
        report["local_operations"] = json!(operations);
    }

    // Shared outputs satisfy the current and next invariants and all activated
    // relations. There is deliberately no blanket output equality on idle steps:
    // private-state stutter does not imply observable stutter for every contract.
    q.query_implication(
        "binding_product_preservation",
        active,
        and(relation, next_invariant),
        &context,
    )?;
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
    Ok(Some(json!({
        "responses":response_results,
        "status": status,
        "composition": implementation.composition,
        "members": product.members,
        "actions": implementation.operations.keys().collect::<Vec<_>>(),
        "obligations": reports,
        "claim": "From reset, the supplied state-only abstraction preserves the relational product and all invariants for every nonreset implementation step, with private-state stuttering for each inactive leaf",
        "limitations": [
            "This is a deterministic safety witness into the relational specification, not equality of behavior sets, abstraction surjectivity, or realization of positive examples",
            progress_limitation,
            "Every mapped-invariant implementation state is checked, including unreachable states; reset has priority and restarts the abstract initial state",
            "Distinct local operations of the same leaf must be exclusive; independent leaf actions and exported groups reaching the same local operation may overlap",
            "Inactive leaves preserve private state; shared outputs remain constrained by invariants and active relations, with no general observable-stutter claim"
        ]
    })))
}
