//! Bounded admission tests for relational component products.
use hwverify_ir::*;
use hwverify_solver::Check;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

fn frame(vars: &Env, kind: &str, time: usize) -> Env {
    vars.iter()
        .enumerate()
        .map(|(index, (name, t))| {
            (
                name.clone(),
                var(format!("trace_{kind}_t{time}_v{index}"), t.0.sort.clone()),
            )
        })
        .collect()
}
fn replace(map: &mut BTreeMap<Term, Term>, from: &Env, to: &Env) {
    map.extend(from.iter().map(|(name, t)| (t.clone(), to[name].clone())));
}
fn restrict(constraints: &mut Vec<Term>, variables: &Env, values: &Env) {
    constraints.extend(
        values
            .iter()
            .map(|(name, value)| eq(variables[name].clone(), value.clone())),
    );
}
fn conjunction(terms: &[Term]) -> Term {
    match terms.len() {
        0 => boolv(true),
        1 => terms[0].clone(),
        n => and(conjunction(&terms[..n / 2]), conjunction(&terms[n / 2..])),
    }
}
/// Constructs the conjunction whose free variables are existentially quantified
/// by SAT. In particular, no private-state or omitted-observation guess is made.
fn trace_admission(
    spec: &Specification,
    members: &[String],
    example: &TraceExample,
) -> Res<(Term, Env)> {
    if members.is_empty() {
        return Err("empty product".into());
    }
    let mut selected = std::collections::BTreeSet::new();
    for name in members {
        if !spec.components().contains_key(name) || !selected.insert(name) {
            return Err(format!("unknown or duplicate component {name}"));
        }
    }
    let mut constraints = vec![];
    let mut context = Env::new();
    let observations = (0..=example.trace.len())
        .map(|time| frame(spec.observations(), "obs", time))
        .collect::<Vec<_>>();
    restrict(&mut constraints, &observations[0], &example.initial);
    for (time, obs) in observations.iter().enumerate() {
        for (name, value) in obs {
            context.insert(format!("frame{time}.o.{name}"), value.clone());
        }
    }
    let inputs = example
        .trace
        .iter()
        .enumerate()
        .map(|(time, step)| {
            let input = frame(spec.inputs(), "input", time);
            for (name, value) in &input {
                context.insert(format!("step{time}.i.{name}"), value.clone());
            }
            restrict(&mut constraints, &input, &step.inputs);
            restrict(&mut constraints, &observations[time + 1], &step.observe);
            input
        })
        .collect::<Vec<_>>();
    for (component_index, name) in members.iter().enumerate() {
        let component = &spec.components()[name];
        let states = (0..=example.trace.len())
            .map(|time| frame(&component.state, &format!("private{component_index}"), time))
            .collect::<Vec<_>>();
        for time in 0..=example.trace.len() {
            let mut replacements = BTreeMap::new();
            replace(&mut replacements, &component.state, &states[time]);
            replace(&mut replacements, spec.observations(), &observations[time]);
            constraints.push(substitute(&component.invariant, &replacements));
            if time == 0 {
                constraints.push(substitute(&component.initial, &replacements));
            }
            for (field, value) in &states[time] {
                context.insert(format!("frame{time}.{name}.{field}"), value.clone());
            }
            if let Some(step) = example.trace.get(time) {
                replace(&mut replacements, &component.next_state, &states[time + 1]);
                replace(
                    &mut replacements,
                    spec.next_observations(),
                    &observations[time + 1],
                );
                replace(&mut replacements, spec.inputs(), &inputs[time]);
                constraints.push(substitute(&component.steps[&step.operation], &replacements));
            }
        }
    }
    Ok((conjunction(&constraints), context))
}

pub fn check_specification(spec: &Specification, z3: String, out: PathBuf) -> Res<Value> {
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut query = Check {
        z3,
        out,
        reports: vec![],
    };
    let mut examples = vec![];
    let mut coverage = vec![];
    for (kind, name, members, cases) in spec
        .components()
        .iter()
        .map(|(n, c)| ("component", n, vec![n.clone()], &c.examples))
        .chain(
            spec.compositions()
                .iter()
                .map(|(n, p)| ("composition", n, p.members.clone(), &p.examples)),
        )
    {
        let positive_count = cases.values().filter(|e| e.positive).count();
        let negative_count = cases.len() - positive_count;
        let mut warnings = vec![];
        if positive_count == 0 {
            warnings.push("No positive examples: passing negative examples does not establish that this target admits any behavior");
        }
        if negative_count == 0 {
            warnings
                .push("No negative examples: no unwanted behavior has been tested for exclusion");
        }
        coverage.push(json!({"target_kind":kind,"target":name,"members":members,"positive_examples":positive_count,"negative_examples":negative_count,"warnings":warnings}));
        for (case, example) in cases {
            let id = format!("example_{}", examples.len());
            let (formula, context) = trace_admission(spec, &members, example)?;
            query.query_with_witness(&id, formula, example.positive, &context)?;
            let evidence = query.reports.last().unwrap();
            let admitted = match evidence["solver_result"].as_str() {
                Some("sat") => Some(true),
                Some("unsat") => Some(false),
                _ => None,
            };
            let status = match admitted {
                Some(value) if value == example.positive => "passed",
                Some(_) => "failed",
                None => "unknown",
            };
            examples.push(json!({"target_kind":kind,"target":name,"members":members,"example":case,"expect":if example.positive {"positive"} else {"negative"},"steps":example.trace.len(),"status":status,"admitted":admitted,"evidence":evidence}));
        }
    }
    let binding = crate::spec_binding::obligations(spec, &mut query)?;
    let binding_status = binding.as_ref().and_then(|b| b["status"].as_str());
    let status = if examples.iter().any(|x| x["status"] == "failed") {
        "spec_examples_failed"
    } else if binding_status == Some("failed") {
        "implementation_binding_failed"
    } else if examples.iter().any(|x| x["status"] == "unknown") || binding_status == Some("unknown")
    {
        "unknown"
    } else if binding_status == Some("verified") {
        if examples.is_empty() {
            "binding_verified_no_examples"
        } else {
            "spec_examples_and_binding_verified"
        }
    } else if examples.is_empty() {
        "no_examples"
    } else {
        "spec_examples_passed"
    };
    Ok(
        json!({"status":status,"name":spec.document().get("name"),"examples":examples,"coverage":coverage,"implementation_binding":binding,"claim":"Example results concern only the supplied finite observational traces; any separate universal safety/stuttering binding result is reported under implementation_binding", "semantics":{"composition":"conjunction with private component state and shared observations/inputs; all members synchronize on every named operation","initial":"component init and invariant at frame 0; no implicit reset operation","timing":"step k consumes inputs k and relates observations/state at frames k and k+1; observe constrains frame k+1","projection":"all hidden private state and omitted inputs/observations are existential; negative examples require UNSAT for every hidden completion"},"limitations":["Passing examples alone is not universal verification, specification adequacy, deadlock freedom, liveness, or implementation refinement; any separate binding proof is reported under implementation_binding","Initial predicates define example starting states; no reset reachability or arbitrary future extension is inferred","Rust lowering, the structural kernel and Z3 are trusted; emitted SMT obligations are replayable"]}),
    )
}
