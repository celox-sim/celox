//! Bounded actual failures from reset. Never seeds a trace from an induction model.
use hwverify_ir::*;
use hwverify_solver::finite::{self, Limits, Scalar, SearchHint, Verdict};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn or(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}
// Preserve every conjunct while avoiding a linear-depth chain of frame equations.
// Association changes only; no transition or property is simplified or assumed.
fn conjunction(terms: &[Term]) -> Term {
    match terms.len() {
        0 => boolv(true),
        1 => terms[0].clone(),
        n => and(conjunction(&terms[..n / 2]), conjunction(&terms[n / 2..])),
    }
}
fn values(v: &BTreeMap<String, Scalar>) -> Value {
    json!(v
        .iter()
        .map(|(k, v)| (k, v.json()))
        .collect::<BTreeMap<_, _>>())
}
fn assignments(env: &Env, v: &BTreeMap<String, Scalar>) -> Res<BTreeMap<String, Scalar>> {
    env.iter()
        .map(|(k, t)| {
            Ok((
                t.0.op
                    .strip_prefix('@')
                    .ok_or("nonvariable state/input")?
                    .into(),
                v.get(k).ok_or("missing value")?.clone(),
            ))
        })
        .collect()
}
fn eval(terms: &Env, a: &BTreeMap<String, Scalar>) -> Res<BTreeMap<String, Scalar>> {
    finite::evaluate_scalar_terms(terms, a, Limits::default())
}
fn input_values(v: &Value, types: &Env) -> Res<BTreeMap<String, Scalar>> {
    let map = v.as_object().ok_or("inputs must be an object")?;
    if map.len() != types.len() || map.keys().any(|k| !types.contains_key(k)) {
        return Err("input key mismatch".into());
    }
    types
        .iter()
        .map(|(k, t)| {
            let value = match t.0.sort {
                Sort::Bool => Scalar::Bool(map[k].as_bool().ok_or("boolean input required")?),
                Sort::Bv(w @ 1..=64) => {
                    let n = map[k].as_u64().ok_or("unsigned input required")?;
                    if w < 64 && n >= (1u64 << w) {
                        return Err("input out of range".into());
                    }
                    Scalar::Bv { width: w, value: n }
                }
                _ => return Err("unsupported input sort".into()),
            };
            Ok((k.clone(), value))
        })
        .collect()
}
fn raw_inputs(v: &BTreeMap<String, Scalar>) -> Value {
    json!(v
        .iter()
        .map(|(k, v)| (
            k,
            match v {
                Scalar::Bool(b) => json!(b),
                Scalar::Bv { value, .. } => json!(value),
            }
        ))
        .collect::<BTreeMap<_, _>>())
}
fn supported(s: &Specification) -> Res<()> {
    if crate::structure::check(s, None)?["status"] != "not_requested" {
        return Err("behavioral replay cannot discharge structural contracts; use the source structural checker separately".into());
    }
    let m = &s.implementation().ok_or("missing implementation")?.machine;
    if m.state
        .values()
        .chain(s.inputs().values())
        .any(|t| !matches!(t.0.sort, Sort::Bool | Sort::Bv(1..=64)))
    {
        return Err("only scalar Bool/BV<=64 replay supported".into());
    }
    Ok(())
}
fn goal(s: &Specification, goal: &str) -> Res<()> {
    if goal == "safety" {
        return Ok(());
    }
    if goal == "response_deadline" && s.implementation().unwrap().responses.len() == 1 {
        return Ok(());
    }
    Err("goal must be safety or response_deadline with one response contract".into())
}

/// Search response deadlines or original relational safety predicates. The finite
/// result is separate from the inductive checker and never strengthens its verdict.
pub fn search_reachable(spec: &Specification, selected: &str, depth: u32) -> Res<Value> {
    search_with_limits(spec, selected, depth, Limits::default())
}
fn search_with_limits(
    spec: &Specification,
    selected: &str,
    depth: u32,
    limits: Limits,
) -> Res<Value> {
    supported(spec)?;
    goal(spec, selected)?;
    if !(1..=32).contains(&depth) {
        return Err("depth must be 1..32".into());
    }
    let i = spec.inputs();
    let imp = spec.implementation().unwrap();
    let m = &imp.machine;
    let mut report = json!({"kind":"bounded_failure_search","goal":selected,"depth":depth,"status":"unknown","witness":null,
        "scope":"one reset edge 0 then at most depth nonreset edges; no arbitrary-state initialization; no unbounded safety/liveness claim"});
    if (i.len() + m.state.len()) * (depth as usize + 1) > 4096 {
        report["reason"] = json!("frame-symbol budget exhausted");
        return Ok(report);
    }
    let safety = crate::spec_binding::safety_model(spec)?;
    let mut context = Env::new();
    let mut frame = |env: &Env, edge: u32, kind: &str| -> Env {
        env.iter()
            .map(|(n, t)| {
                let key = format!("edge{edge}.{kind}.{n}");
                let v = var(key.clone(), t.0.sort.clone());
                context.insert(key, v.clone());
                (n.clone(), v)
            })
            .collect()
    };
    let ins = (0..=depth)
        .map(|e| frame(i, e, "input"))
        .collect::<Vec<_>>();
    let states = (0..=depth)
        .map(|e| frame(&m.state, e, "after"))
        .collect::<Vec<_>>();
    let sub_for = |edge: usize| -> BTreeMap<Term, Term> {
        let mut sub = i
            .iter()
            .map(|(n, t)| (t.clone(), ins[edge][n].clone()))
            .collect::<BTreeMap<_, _>>();
        if edge > 0 {
            sub.extend(
                m.state
                    .iter()
                    .map(|(n, t)| (t.clone(), states[edge - 1][n].clone())),
            );
        }
        sub
    };
    let mut reset_constraints = vec![ins[0][&imp.reset_input].clone()];
    let sub = sub_for(0);
    for (n, t) in &m.reset {
        reset_constraints.push(eq(states[0][n].clone(), substitute(t, &sub)));
    }
    let mut prefix = conjunction(&reset_constraints);
    let mut formula = if selected == "safety" {
        not(substitute(&safety.reset_good, &sub))
    } else {
        boolv(false)
    };
    let contract = imp.responses.values().next();
    let mut pending = boolv(false);
    let mut age = bv(64, 0);
    for e in 1..=depth as usize {
        let sub = sub_for(e);
        let mut edge_constraints = vec![not(ins[e][&imp.reset_input].clone())];
        for (n, t) in &m.next {
            edge_constraints.push(eq(states[e][n].clone(), substitute(t, &sub)));
        }
        prefix = and(prefix, conjunction(&edge_constraints));
        let bad = if selected == "safety" {
            safety.checks.iter().fold(boolv(false), |acc, (_, bad, _)| {
                or(acc, substitute(bad, &sub))
            })
        } else {
            let c = contract.unwrap();
            let assume = substitute(&c.assumption, &sub);
            let accept = substitute(&c.accept, &sub);
            let complete = substitute(&imp.operations[&c.operation], &sub);
            let bad = and(
                assume.clone(),
                and(
                    pending.clone(),
                    and(
                        not(complete.clone()),
                        not(node(
                            Sort::Bool,
                            "bvult",
                            vec![age.clone(), bv(64, c.bound - 1)],
                        )),
                    ),
                ),
            );
            let next_age = ite(
                pending.clone(),
                node(Sort::Bv(64), "bvadd", vec![age, bv(64, 1)]),
                bv(64, 0),
            );
            pending = and(assume, and(not(complete), or(pending, accept)));
            age = next_age;
            bad
        };
        formula = or(formula, bad);
    }
    // Every machine state has a total next expression over finite scalar sorts.
    // Thus an earlier failing prefix extends to this bound for arbitrary future
    // nonreset inputs. Conjoin the full trajectory once, outside the failure OR:
    // this preserves existence of a failure and exposes mandatory frame equations
    // to the solver, without assuming any specification invariant or guarantee.
    let formula = and(prefix, formula);
    let solved = finite::solve_with_hint(&formula, &context, limits, SearchHint::Sat);
    report["solver"] = solved.diagnostics();
    match solved.verdict {
        Verdict::Unknown => report["reason"] = json!(solved.reason),
        Verdict::Unsat => report["status"] = json!("bounded_no_failure"),
        Verdict::Sat => {
            if !solved.original_formula_validated {
                report["reason"] = json!("unvalidated solver model");
                return Ok(report);
            }
            let mut inputs = vec![];
            for e in 0..=depth {
                let row = i
                    .keys()
                    .map(|n| {
                        Ok((
                            n.clone(),
                            solved
                                .context_values
                                .get(&format!("edge{e}.input.{n}"))
                                .ok_or("missing model input")?
                                .clone(),
                        ))
                    })
                    .collect::<Res<BTreeMap<_, _>>>()?;
                inputs.push(raw_inputs(&row));
            }
            // The solver models the full trajectory, including later frames.
            // Replay stops at its first actual failure and ignores later inputs.
            match execute(spec, selected, &inputs, true) {
                Ok(trace) => {
                    for f in &trace {
                        let e = f["edge"].as_u64().unwrap();
                        for n in m.state.keys() {
                            if f["state_after"][n]
                                != solved.context_values[&format!("edge{e}.after.{n}")].json()
                            {
                                return Err(
                                    "SAT state disagrees with original transition replay".into()
                                );
                            }
                        }
                    }
                    report["status"] = json!("reset_reachable_failure");
                    report["witness"] = json!({"version":1,"kind":"reset_reachable_failure","goal":selected,"depth":depth,
                        "document":spec.document(),"trace":trace,"original_transitions_and_property_validated":true});
                }
                Err(e) => report["reason"] = json!(format!("SAT replay rejected: {e}")),
            }
        }
    }
    Ok(report)
}

/// Recompute from original expressions; neither supplied states nor success flags
/// are assumptions. Positive covers and induction countermodels are not witnesses.
pub fn validate_reachable(spec: &Specification, witness: &Value) -> Res<Value> {
    supported(spec)?;
    if witness["version"] != 1
        || witness["kind"] != "reset_reachable_failure"
        || witness["document"] != *spec.document()
    {
        return Err("stale model or unsupported witness identity/kind".into());
    }
    let selected = witness["goal"].as_str().ok_or("missing goal")?;
    goal(spec, selected)?;
    let depth = witness["depth"]
        .as_u64()
        .filter(|d| (1..=32).contains(d))
        .ok_or("invalid witness depth")?;
    let supplied = witness["trace"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= depth as usize + 1)
        .ok_or("invalid trace length")?;
    let inputs = supplied
        .iter()
        .map(|f| f["inputs"].clone())
        .collect::<Vec<_>>();
    let trace = execute(spec, selected, &inputs, true)?;
    if *supplied != trace {
        return Err("trace state, property or indexing mismatch".into());
    }
    Ok(
        json!({"status":"reset_reachable_failure","original_transitions_and_property_validated":true,"trace":trace}),
    )
}
fn execute(
    spec: &Specification,
    selected: &str,
    inputs: &[Value],
    require_failure: bool,
) -> Res<Vec<Value>> {
    let imp = spec.implementation().unwrap();
    let m = &imp.machine;
    let safety = crate::spec_binding::safety_model(spec)?;
    let mut state = BTreeMap::new();
    let mut trace = vec![];
    let mut pending = false;
    let mut age = 0u64;
    for (e, row) in inputs.iter().enumerate() {
        let input = input_values(row, spec.inputs())?;
        if input[&imp.reset_input] != Scalar::Bool(e == 0) {
            return Err("reset must occur exactly at edge 0".into());
        }
        let mut a = assignments(spec.inputs(), &input)?;
        if e > 0 {
            a.extend(assignments(&m.state, &state)?);
        }
        let next = eval(if e == 0 { &m.reset } else { &m.next }, &a)?;
        let mut property = None;
        let mut controls = Value::Null;
        if selected == "safety" {
            if e == 0 {
                if eval(&Env::from([("good".into(), safety.reset_good.clone())]), &a)?["good"]
                    != Scalar::Bool(true)
                {
                    property = Some("binding_reset_establishes_product".to_owned());
                }
            } else {
                for (name, bad, _) in &safety.checks {
                    if eval(&Env::from([("bad".into(), bad.clone())]), &a)?["bad"]
                        == Scalar::Bool(true)
                    {
                        property = Some(name.clone());
                        break;
                    }
                }
            }
        } else if e > 0 {
            let c = imp.responses.values().next().unwrap();
            let v = eval(
                &Env::from([
                    ("assume".into(), c.assumption.clone()),
                    ("accept".into(), c.accept.clone()),
                    ("complete".into(), imp.operations[&c.operation].clone()),
                ]),
                &a,
            )?;
            let assume = v["assume"] == Scalar::Bool(true);
            let accept = v["accept"] == Scalar::Bool(true);
            let complete = v["complete"] == Scalar::Bool(true);
            controls = json!({"assume":assume,"accept":accept,"complete":complete,"monitor_pending":pending,"monitor_age":age});
            if !assume || complete {
                pending = false;
                age = 0;
            } else if pending {
                age += 1;
                if age >= c.bound {
                    property = Some("response_deadline".into());
                }
            } else if accept {
                pending = true;
                age = 0;
            }
        }
        trace.push(json!({"edge":e,"inputs":row,"state_before":if e==0 {Value::Null} else {values(&state)},"state_after":values(&next),"controls":controls,"violation":property}));
        state = next;
        if property.is_some() {
            return Ok(trace);
        }
    }
    if require_failure {
        return Err("trace contains no actual property violation".into());
    }
    Ok(trace)
}

/// Execute explicit stimuli on the original machine/property. Useful for checking
/// a saved regression against a corrected design without treating DUT outputs as expected values.
pub fn check_stimulus(spec: &Specification, selected: &str, inputs: &[Value]) -> Res<Value> {
    supported(spec)?;
    goal(spec, selected)?;
    if inputs.is_empty() || inputs.len() > 33 {
        return Err("stimulus must contain 1..33 frames".into());
    }
    let trace = execute(spec, selected, inputs, false)?;
    let failed = !trace.last().unwrap()["violation"].is_null();
    Ok(
        json!({"status":if failed {"reset_reachable_failure"} else {"trace_no_failure"},"trace":trace}),
    )
}

/// Observe explicitly named deterministic DUT wires during an original-property
/// replay. These are simulation comparison values, never a relational-spec oracle.
pub fn observe_stimulus(
    spec: &Specification,
    selected: &str,
    inputs: &[Value],
    signals: &[String],
) -> Res<Value> {
    let mut result = check_stimulus(spec, selected, inputs)?;
    let imp = spec.implementation().unwrap();
    let terms = signals
        .iter()
        .map(|name| {
            Ok((
                name.clone(),
                imp.wires
                    .get(name)
                    .ok_or(format!("unknown DUT wire {name}"))?
                    .clone(),
            ))
        })
        .collect::<Res<Env>>()?;
    if terms.len() != signals.len() {
        return Err("duplicate sampled wire".into());
    }
    for frame in result["trace"].as_array_mut().unwrap() {
        if frame["edge"] == 0 {
            continue;
        }
        let iv = input_values(&frame["inputs"], spec.inputs())?;
        let raw = frame["state_before"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v["value"].clone()))
            .collect::<serde_json::Map<_, _>>();
        let sv = input_values(&Value::Object(raw), &imp.machine.state)?;
        let mut a = assignments(spec.inputs(), &iv)?;
        a.extend(assignments(&imp.machine.state, &sv)?);
        frame["signal_samples"] = values(&eval(&terms, &a)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> Value {
        hwverify_syntax::parse_document(include_str!("../../../examples/response.hwv"), "test.hwv")
            .unwrap()
            .canonical
    }
    fn spec(v: &Value) -> Specification {
        Specification::from_json(v).unwrap()
    }
    fn inputs(rows: &[(bool, bool)]) -> Vec<Value> {
        std::iter::once(json!({"rst":true,"request":false,"stall":true}))
            .chain(
                rows.iter()
                    .map(|(request, stall)| json!({"rst":false,"request":request,"stall":stall})),
            )
            .collect()
    }
    #[test]
    fn balanced_conjunction_preserves_all_leaves_and_empty_identity() {
        let terms = (0..7)
            .map(|n| var(format!("b{n}"), Sort::Bool))
            .collect::<Vec<_>>();
        let balanced = conjunction(&terms);
        let linear = terms.iter().cloned().fold(boolv(true), and);
        let env = [("balanced".into(), balanced), ("linear".into(), linear)]
            .into_iter()
            .collect();
        for bits in 0..128 {
            let inputs = (0..7)
                .map(|n| (format!("b{n}"), Scalar::Bool(bits & (1 << n) != 0)))
                .collect();
            let values = eval(&env, &inputs).unwrap();
            assert_eq!(values["balanced"], Scalar::Bool(bits == 127));
            assert_eq!(values["balanced"], values["linear"]);
        }
        assert_eq!(conjunction(&[]), boolv(true));
    }
    #[test]
    fn wide_frames_keep_reset_transitions_and_last_edge_failures() {
        let mut fields = serde_json::Map::new();
        let mut reset = serde_json::Map::new();
        let mut next = serde_json::Map::new();
        let mut bindings = serde_json::Map::new();
        let mut initial = json!(true);
        let mut step = json!(true);
        for n in 0..64 {
            let name = format!("v{n}");
            fields.insert(name.clone(), json!({"bv":4}));
            reset.insert(name.clone(), json!(["bv", 4, 0]));
            next.insert(name.clone(), json!(format!("s.{name}")));
            bindings.insert(name.clone(), json!(format!("s.{name}")));
            initial = json!(["and", initial, ["eq", format!("s.{name}"), ["bv", 4, 0]]]);
            step = json!([
                "and",
                step,
                ["eq", format!("n.{name}"), format!("s.{name}")]
            ]);
        }
        let mut machine_fields = fields.clone();
        machine_fields.insert("timer".into(), json!({"bv":4}));
        reset.insert("timer".into(), json!(["bv", 4, 0]));
        next.insert("timer".into(), json!(["add", "s.timer", ["bv", 4, 1]]));
        let mut d = json!({"version":3,"kind":"specification","name":"wide frame regression",
            "inputs":{"rst":"bool"},"observations":{},"operations":{"tick":{}},
            "components":{"Hold":{"state":fields,"init":initial,"invariant":true,"steps":{"tick":step},"examples":{}}},
            "compositions":{"System":{"members":["Hold"],"examples":{}}},
            "implementation":{"composition":"System","reset_input":"rst","state":machine_fields,"reset":reset,"next":next,"wires":{},"operations":{"tick":true},"binding":{"states":{"Hold":bindings},"observations":{}}}});
        assert_eq!(
            search_reachable(&spec(&d), "safety", 10).unwrap()["status"],
            "bounded_no_failure"
        );
        // First/middle/last equation mutations must not be dropped when balancing.
        for field in ["v0", "v31", "v63"] {
            d["implementation"]["next"][field] = json!([
                "ite",
                ["eq", "s.timer", ["bv", 4, 9]],
                ["bv", 4, 1],
                format!("s.{field}")
            ]);
            assert_eq!(
                search_reachable(&spec(&d), "safety", 9).unwrap()["status"],
                "bounded_no_failure"
            );
            let s = spec(&d);
            let failure = search_reachable(&s, "safety", 10).unwrap();
            assert_eq!(failure["status"], "reset_reachable_failure");
            validate_reachable(&s, &failure["witness"]).unwrap();
            assert_eq!(
                failure["witness"]["trace"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["edge"],
                10
            );
            d["implementation"]["next"][field] = json!(format!("s.{field}"));
            d["implementation"]["reset"][field] = json!(["bv", 4, 1]);
            assert_eq!(
                search_reachable(&spec(&d), "safety", 10).unwrap()["witness"]["trace"][0]["edge"],
                0
            );
            d["implementation"]["reset"][field] = json!(["bv", 4, 0]);
        }
        let limited = search_with_limits(
            &spec(&d),
            "safety",
            10,
            Limits {
                max_depth: 4,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(limited["status"], "unknown");
    }
    #[test]
    fn complete_trajectory_query_matches_exhaustive_prefix_failures() {
        // The specification may already fail at reset or an early edge. Future
        // equations must not silently assume its invariant or require progress.
        for target in 0..4u64 {
            let d = json!({"version":3,"kind":"specification","name":"total scalar trajectory",
                "inputs":{"rst":"bool","choose":"bool"},"observations":{},"operations":{"tick":{}},
                "components":{"Check":{"state":{"value":{"bv":2}},"init":["eq","s.value",["bv",2,0]],
                    "invariant":["not",["eq","s.value",["bv",2,target]]],"steps":{"tick":true},"examples":{}}},
                "compositions":{"System":{"members":["Check"],"examples":{}}},
                "implementation":{"composition":"System","reset_input":"rst","state":{"value":{"bv":2}},
                    "reset":{"value":["bv",2,0]},"next":{"value":["ite","i.choose",["add","s.value",["bv",2,1]],"s.value"]},
                    "wires":{},"operations":{"tick":true},"binding":{"states":{"Check":{"value":"s.value"}},"observations":{}}}});
            let s = spec(&d);
            for depth in 1..=4 {
                let mut any_failure = false;
                for choices in 0..(1 << depth) {
                    let mut trace = vec![json!({"rst":true,"choose":false})];
                    let mut count = 0;
                    let mut failed = target == 0;
                    for edge in 0..depth {
                        let choose = choices & (1 << edge) != 0;
                        if choose {
                            count = (count + 1) % 4;
                        }
                        failed |= count == target;
                        trace.push(json!({"rst":false,"choose":choose}));
                    }
                    let actual = check_stimulus(&s, "safety", &trace).unwrap();
                    assert_eq!(actual["status"] == "reset_reachable_failure", failed);
                    any_failure |= failed;
                }
                let result = search_reachable(&s, "safety", depth).unwrap();
                assert_eq!(
                    result["status"],
                    if any_failure {
                        "reset_reachable_failure"
                    } else {
                        "bounded_no_failure"
                    }
                );
                if any_failure {
                    validate_reachable(&s, &result["witness"]).unwrap();
                }
            }
        }
    }
    #[test]
    fn early_deadline_failure_survives_later_cancellation_or_completion() {
        for cancellation in [false, true] {
            let mut d = document();
            d["implementation"]["next"]["count"] = json!(["add", "s.count", ["bv", 4, 1]]);
            d["implementation"]["responses"]["request_done"]["accept"] =
                json!(["eq", "s.count", ["bv", 4, 0]]);
            d["implementation"]["responses"]["request_done"]["assume"] = if cancellation {
                json!(["not", "i.stall"])
            } else {
                json!(true)
            };
            d["implementation"]["operations"]["advance"] = if cancellation {
                json!(false)
            } else {
                json!(["eq", "s.count", ["bv", 4, 3]])
            };
            let s = spec(&d);
            // Acceptance at edge 1 expires at edge 3. At edge 4 the interval
            // cancels/completes, but extending to edge 6 must retain the failure.
            let mut extension = [(false, false); 6];
            if cancellation {
                extension[3].1 = true;
            }
            let concrete = check_stimulus(&s, "response_deadline", &inputs(&extension)).unwrap();
            assert_eq!(concrete["status"], "reset_reachable_failure");
            assert_eq!(
                concrete["trace"].as_array().unwrap().last().unwrap()["edge"],
                3
            );
            let result = search_reachable(&s, "response_deadline", 6).unwrap();
            assert_eq!(result["status"], "reset_reachable_failure");
            assert_eq!(
                result["witness"]["trace"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["edge"],
                3
            );
            validate_reachable(&s, &result["witness"]).unwrap();
        }
    }
    #[test]
    fn sampled_dut_signals_use_preedge_state_and_reject_bad_names() {
        let s = spec(&document());
        let rows = inputs(&[(true, false), (false, false), (false, false)]);
        let sampled = observe_stimulus(
            &s,
            "response_deadline",
            &rows,
            &["accept".into(), "complete".into()],
        )
        .unwrap();
        assert_eq!(
            sampled["trace"][1]["signal_samples"]["accept"]["value"],
            true
        );
        assert_eq!(sampled["trace"][1]["state_after"]["busy"]["value"], true);
        assert_eq!(
            sampled["trace"][3]["signal_samples"]["complete"]["value"],
            true
        );
        assert!(observe_stimulus(&s, "response_deadline", &rows, &["missing".into()]).is_err());
        assert!(observe_stimulus(
            &s,
            "response_deadline",
            &rows,
            &["accept".into(), "accept".into()]
        )
        .is_err());
    }
    #[test]
    fn deadline_counts_subsequent_edges_and_ignores_dut_pending_and_rank() {
        let mut d = document();
        d["implementation"]["next"]["busy"] = json!(false);
        d["implementation"]["responses"]["request_done"]["pending"] = json!(false);
        d["implementation"]["responses"]["request_done"]["rank"] = json!(["bv", 2, 0]);
        let s = spec(&d);
        let trace = inputs(&[(true, false), (false, false), (false, false)]);
        assert_eq!(
            check_stimulus(&s, "response_deadline", &trace[..3]).unwrap()["status"],
            "trace_no_failure"
        );
        let r = check_stimulus(&s, "response_deadline", &trace).unwrap();
        assert_eq!(r["status"], "reset_reachable_failure");
        assert_eq!(r["trace"][3]["controls"]["monitor_age"], 1);
        assert_eq!(
            check_stimulus(&spec(&document()), "response_deadline", &trace).unwrap()["status"],
            "trace_no_failure"
        );
        let sat = search_reachable(&s, "response_deadline", 3).unwrap();
        assert_eq!(sat["status"], "reset_reachable_failure");
        validate_reachable(&s, &sat["witness"]).unwrap();
    }
    #[test]
    fn assumptions_cancel_only_affected_intervals_and_reset_is_not_assumed() {
        let mut d = document();
        d["implementation"]["next"]["busy"] = json!(false);
        let s = spec(&d);
        // An assumption violation AFTER acceptance cancels the guarantee.
        assert_eq!(
            check_stimulus(
                &s,
                "response_deadline",
                &inputs(&[(true, false), (false, true), (false, false), (false, false)])
            )
            .unwrap()["status"],
            "trace_no_failure"
        );
        // Prior violations do not exempt a later qualifying acceptance.
        assert_eq!(
            check_stimulus(
                &s,
                "response_deadline",
                &inputs(&[(false, true), (true, false), (false, false), (false, false)])
            )
            .unwrap()["status"],
            "reset_reachable_failure"
        );
        // Same-edge completion discharges a freshly accepted request.
        d["implementation"]["operations"]["advance"] = json!("w.accept");
        assert_eq!(
            check_stimulus(
                &spec(&d),
                "response_deadline",
                &inputs(&[(true, false), (false, false), (false, false)])
            )
            .unwrap()["status"],
            "trace_no_failure"
        );
    }
    #[test]
    fn replay_rejects_positive_inductive_stale_corrupt_and_nonreset_traces() {
        let mut d = document();
        d["implementation"]["next"]["count"] = json!(["bv", 4, 3]);
        let s = spec(&d);
        let result = search_reachable(&s, "safety", 2).unwrap();
        assert_eq!(result["status"], "reset_reachable_failure");
        let original = result["witness"].clone();
        validate_reachable(&s, &original).unwrap();
        for kind in ["positive_cover", "induction_countermodel"] {
            let mut w = original.clone();
            w["kind"] = json!(kind);
            assert!(validate_reachable(&s, &w).is_err());
        }
        let mut w = original.clone();
        w["document"]["name"] = json!("stale");
        assert!(validate_reachable(&s, &w).is_err());
        let mut w = original.clone();
        w["trace"][0]["state_after"]["count"]["value"] = json!(9);
        assert!(validate_reachable(&s, &w).is_err());
        let mut w = original.clone();
        w["trace"][0]["inputs"]["rst"] = json!(false);
        assert!(validate_reachable(&s, &w).is_err());
        let mut w = original;
        w["trace"].as_array_mut().unwrap().push(json!({"edge":99}));
        assert!(validate_reachable(&s, &w).is_err());
    }
    #[test]
    fn safety_ignores_response_assumptions_and_unknown_is_not_failure_or_proof() {
        let mut d = document();
        d["implementation"]["next"]["count"] = json!(["bv", 4, 3]);
        d["implementation"]["responses"]["request_done"]["assume"] = json!(false);
        let s = spec(&d);
        assert_eq!(
            search_reachable(&s, "safety", 2).unwrap()["status"],
            "reset_reachable_failure"
        );
        assert_eq!(
            search_reachable(&s, "response_deadline", 2).unwrap()["status"],
            "bounded_no_failure"
        );
        let unknown = search_with_limits(
            &s,
            "safety",
            2,
            Limits {
                max_work: 0,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(unknown["status"], "unknown");
        assert!(unknown["witness"].is_null());
    }
}
