//! Bounded existential adequacy evidence, separate from response proof obligations.
use hwverify_ir::*;
use hwverify_solver::finite::{self, Limits, Scalar, SearchHint, Verdict};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn check(
    machine: &SpecImplementation,
    inputs: &Env,
    contract: &BoundedResponse,
) -> Value {
    let Some(depth) = contract.cover_depth else {
        return json!({"status":"unchecked","reason":"cover_depth not specified"});
    };
    check_with_limits(machine, inputs, contract, depth, Limits::default())
}

fn check_with_limits(
    implementation: &SpecImplementation,
    inputs: &Env,
    c: &BoundedResponse,
    depth: u32,
    limits: Limits,
) -> Value {
    let mut report = json!({"status":"unknown","depth":depth,
        "scope":"One canonical reset edge 0, then acceptance on one of nonreset edges 1..depth; input assumption holds on every nonreset edge through acceptance, not on reset",
        "claim":"Existence of an accepting input sequence only; not universal request service or unbounded reachability",
        "witness":null,"limits":{"timeout_ms":limits.timeout_ms,"max_terms":limits.max_terms,
            "max_variables":limits.max_variables,"max_clauses":limits.max_clauses,"max_work":limits.max_work,
            "max_expression_depth":limits.max_depth,"max_frame_symbols":4096}});
    let m = &implementation.machine;
    // Keep this initial replay format scalar and bounded. Unsupported state is
    // not projected away: that could manufacture a spurious executable witness.
    if m.state
        .values()
        .chain(inputs.values())
        .any(|t| !matches!(t.0.sort, Sort::Bool | Sort::Bv(1..=64)))
    {
        report["reason"] = json!("cover replay supports only Bool and bitvectors of width 1..64");
        return report;
    }
    if (m.state.len() + inputs.len()).saturating_mul(depth as usize + 2) > 4096 {
        report["reason"] = json!("cover frame-symbol budget exceeds 4096");
        return report;
    }
    let mut context = Env::new();
    let input_frames = (0..=depth)
        .map(|edge| frame(inputs, &format!("edge{edge}.input"), &mut context))
        .collect::<Vec<_>>();
    let states = (0..=depth)
        .map(|edge| frame(&m.state, &format!("edge{edge}.state_after"), &mut context))
        .collect::<Vec<_>>();
    let reset_sub = substitutions(inputs, &input_frames[0]);
    let mut prefix = substitute(&inputs[&implementation.reset_input], &reset_sub);
    for (name, term) in &m.reset {
        prefix = and(
            prefix,
            eq(states[0][name].clone(), substitute(term, &reset_sub)),
        );
    }
    let mut formula = boolv(false);
    for edge in 1..=depth as usize {
        let mut sub = substitutions(inputs, &input_frames[edge]);
        sub.extend(substitutions(&m.state, &states[edge - 1]));
        prefix = and(
            prefix,
            not(input_frames[edge][&implementation.reset_input].clone()),
        );
        prefix = and(prefix, substitute(&c.assumption, &sub));
        for (name, term) in &m.next {
            prefix = and(
                prefix,
                eq(states[edge][name].clone(), substitute(term, &sub)),
            );
        }
        // Disjoin prefixes: no constraints after the accepted edge are required.
        formula = node(
            Sort::Bool,
            "or",
            vec![formula, and(prefix.clone(), substitute(&c.accept, &sub))],
        );
    }
    let outcome = finite::solve_with_hint(&formula, &context, limits.clone(), SearchHint::Sat);
    report["solver"] = outcome.diagnostics();
    match outcome.verdict {
        Verdict::Unsat => report["status"] = json!("not_reached_within_bound"),
        Verdict::Unknown => report["reason"] = json!(outcome.reason),
        Verdict::Sat => {
            if !outcome.original_formula_validated {
                report["reason"] =
                    json!("SAT assignment was not validated against original cover formula");
            } else {
                match replay(
                    implementation,
                    inputs,
                    c,
                    depth,
                    &outcome.context_values,
                    limits,
                ) {
                    Ok(witness) => {
                        report["status"] = json!("reached");
                        report["witness"] = witness;
                    }
                    Err(reason) => {
                        report["reason"] =
                            json!(format!("original transition replay failed: {reason}"))
                    }
                }
            }
        }
    }
    report
}

fn frame(original: &Env, prefix: &str, context: &mut Env) -> Env {
    original
        .iter()
        .map(|(name, t)| {
            let key = format!("{prefix}.{name}");
            let term = var(key.clone(), t.0.sort.clone());
            context.insert(key, term.clone());
            (name.clone(), term)
        })
        .collect()
}
fn substitutions(original: &Env, frame: &Env) -> BTreeMap<Term, Term> {
    original
        .iter()
        .map(|(k, v)| (v.clone(), frame[k].clone()))
        .collect()
}
fn values_json(values: &BTreeMap<String, Scalar>) -> Value {
    json!(values
        .iter()
        .map(|(k, v)| (k, v.json()))
        .collect::<BTreeMap<_, _>>())
}

/// Recompute the reset and every next state using the ORIGINAL expressions and
/// concrete inputs, then compare to the SAT model's frames. No unrolled formula,
/// substitution, CNF or solver search is used by this replay.
fn replay(
    implementation: &SpecImplementation,
    inputs: &Env,
    c: &BoundedResponse,
    depth: u32,
    model: &BTreeMap<String, Scalar>,
    limits: Limits,
) -> Res<Value> {
    let m = &implementation.machine;
    let mut state: BTreeMap<String, Scalar> = BTreeMap::new();
    let mut trace = vec![];
    for edge in 0..=depth {
        let input_values = inputs
            .keys()
            .map(|name| {
                let key = format!("edge{edge}.input.{name}");
                Ok((
                    name.clone(),
                    model.get(&key).cloned().ok_or(format!("missing {key}"))?,
                ))
            })
            .collect::<Res<BTreeMap<_, _>>>()?;
        if input_values[&implementation.reset_input] != Scalar::Bool(edge == 0) {
            return Err(format!("reset polarity at edge {edge}"));
        }
        let mut assignment = BTreeMap::new();
        for (name, term) in inputs {
            assignment.insert(
                term.0
                    .op
                    .strip_prefix('@')
                    .ok_or("input is not a variable")?
                    .to_owned(),
                input_values[name].clone(),
            );
        }
        for (name, term) in &m.state {
            if edge > 0 {
                assignment.insert(
                    term.0
                        .op
                        .strip_prefix('@')
                        .ok_or("state is not a variable")?
                        .to_owned(),
                    state[name].clone(),
                );
            }
        }
        let expressions = if edge == 0 {
            m.reset.clone()
        } else {
            m.next.clone()
        };
        // Separate evaluation avoids collisions with user state names.
        let controls = if edge > 0 {
            finite::evaluate_scalar_terms(
                &Env::from([
                    ("assume".into(), c.assumption.clone()),
                    ("accept".into(), c.accept.clone()),
                ]),
                &assignment,
                limits.clone(),
            )?
        } else {
            BTreeMap::new()
        };
        if edge > 0 && controls["assume"] != Scalar::Bool(true) {
            return Err(format!("input assumption at edge {edge}"));
        }
        let next = finite::evaluate_scalar_terms(&expressions, &assignment, limits.clone())?;
        for (name, value) in &next {
            if model.get(&format!("edge{edge}.state_after.{name}")) != Some(value) {
                return Err(format!("state {name} mismatch at edge {edge}"));
            }
        }
        let accepted = edge > 0 && controls["accept"] == Scalar::Bool(true);
        trace.push(json!({"edge":edge,"kind":if edge==0 {"reset"} else {"nonreset"},
            "inputs":values_json(&input_values),"state_before":if edge==0 {Value::Null} else {values_json(&state)},
            "state_after":values_json(&next),"accept":accepted,
            "assumption":if edge==0 {Value::Null} else {json!(true)}}));
        state = next;
        if accepted {
            return Ok(
                json!({"acceptance_edge":edge,"original_transitions_validated":true,"trace":trace}),
            );
        }
    }
    Err("no acceptance in replayed prefix".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn example() -> Specification {
        Specification::from_json(
            &hwverify_syntax::parse_document(
                include_str!("../../../examples/response.hwv"),
                "response.hwv",
            )
            .unwrap()
            .canonical,
        )
        .unwrap()
    }
    #[test]
    fn budget_exhaustion_stays_unknown_and_never_yields_a_witness() {
        let spec = example();
        let implementation = spec.implementation().unwrap();
        let c = &implementation.responses["request_done"];
        let result = check_with_limits(
            implementation,
            spec.inputs(),
            c,
            2,
            Limits {
                max_work: 0,
                ..Limits::default()
            },
        );
        assert_eq!(result["status"], "unknown");
        assert!(result["witness"].is_null());
        assert!(result["reason"].as_str().unwrap().contains("budget"));
    }
    #[test]
    fn original_transition_replay_rejects_corrupt_state_input_and_reset() {
        let spec = example();
        let implementation = spec.implementation().unwrap();
        let c = &implementation.responses["request_done"];
        let result = check(implementation, spec.inputs(), c);
        assert_eq!(result["status"], "reached");
        let model = result["solver"]["context_values"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    if v["sort"] == "Bool" {
                        Scalar::Bool(v["value"].as_bool().unwrap())
                    } else {
                        Scalar::Bv {
                            width: v["width"].as_u64().unwrap() as u32,
                            value: v["value"].as_u64().unwrap(),
                        }
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert!(replay(
            implementation,
            spec.inputs(),
            c,
            2,
            &model,
            Limits::default()
        )
        .is_ok());
        for (key, value) in [
            ("edge0.state_after.busy", Scalar::Bool(true)),
            ("edge0.input.rst", Scalar::Bool(false)),
            ("edge1.input.rst", Scalar::Bool(true)),
            ("edge1.input.stall", Scalar::Bool(true)),
        ] {
            let mut corrupt = model.clone();
            corrupt.insert(key.into(), value);
            assert!(
                replay(
                    implementation,
                    spec.inputs(),
                    c,
                    2,
                    &corrupt,
                    Limits::default()
                )
                .is_err(),
                "{key}"
            );
        }
        let mut missing = model;
        missing.remove("edge0.input.request");
        assert!(replay(
            implementation,
            spec.inputs(),
            c,
            2,
            &missing,
            Limits::default()
        )
        .is_err());
    }
}
