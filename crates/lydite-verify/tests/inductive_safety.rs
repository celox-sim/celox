use lydite_ir::Specification;
use lydite_verify::induction::check_inductive_safety;
use serde_json::{Value, json};
use std::{fs, process::Command};
fn document() -> Value {
    json!({"version":3,"kind":"specification","name":"Two-register strengthening",
        "inputs":{"rst":"bool","input":"bool"},"observations":{},"operations":{"tick":{}},
        "components":{"Check":{"state":{"a":"bool","b":"bool"},"init":true,"invariant":true,
            "steps":{"tick":["not","n.b"]},"examples":{}}},
        "compositions":{"System":{"members":["Check"],"examples":{}}},
        "implementation":{"composition":"System","reset_input":"rst","state":{"a":"bool","b":"bool"},
            "reset":{"a":false,"b":false},"next":{"a":false,"b":"s.a"},"wires":{},"operations":{"tick":true},
            "binding":{"states":{"Check":{"a":"s.a","b":"s.b"}},"observations":{}}}})
}
fn candidate(name: &str, predicate: Value, deps: Vec<&str>) -> Value {
    json!({"name":name,"predicate":predicate,"depends_on":deps})
}
#[test]
fn live_induction_rejects_unchecked_strengthening_and_keeps_original_targets() {
    if std::env::var("LYDITE_INDUCTION_TEST_CHILD").is_err() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "live_induction_rejects_unchecked_strengthening_and_keeps_original_targets",
                "--nocapture",
            ])
            .env("LYDITE_INDUCTION_TEST_CHILD", "1")
            .env("LYDITE_SOLVER", "finite")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let out = std::env::temp_dir().join(format!("lydite-induction-tests-{}", std::process::id()));
    let run = |name: &str, doc: &Value, proposals: &Value| {
        check_inductive_safety(
            &Specification::from_json(doc).unwrap(),
            proposals,
            out.join(name),
        )
    };
    let d = document();
    let a = candidate("a_false", json!(["not", "s.a"]), vec![]);
    let b = candidate("b_false", json!(["not", "s.b"]), vec!["a_false"]);
    let good = run("good", &d, &json!([a, b])).unwrap();
    assert_eq!(good["status"], "inductive_safety_verified");
    assert_eq!(good["candidates"].as_array().unwrap().len(), 2);
    assert!(
        good["proofs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|q| q["backend"] == "checked_proof_bundle")
            .all(|q| q["root"].is_number() && q["coverage"]["fresh_handles_only"] == true)
    );
    let bare = run("bare", &d, &json!([])).unwrap();
    assert_eq!(bare["status"], "induction_counterexample");
    let weak = run("weak", &d, &json!([candidate("weak", json!(true), vec![])])).unwrap();
    assert_eq!(weak["candidates"][0]["status"], "established");
    assert_eq!(weak["status"], "induction_counterexample");
    let wrong_init = run(
        "wrong-init",
        &d,
        &json!([candidate("uninitialized", json!("s.a"), vec![])]),
    )
    .unwrap();
    assert_eq!(wrong_init["status"], "induction_counterexample");
    assert_eq!(wrong_init["candidates"][0]["initialization_checked"], false);
    assert_eq!(wrong_init["target_uses"], json!([]));
    let no_dependency = run(
        "missing-dependency",
        &d,
        &json!([candidate("b", json!(["not", "s.b"]), vec![])]),
    )
    .unwrap();
    assert_eq!(
        no_dependency["candidates"][0]["initialization_checked"],
        true
    );
    assert_eq!(
        no_dependency["candidates"][0]["preservation_checked"],
        false
    );
    assert_eq!(no_dependency["target_uses"], json!([]));
    // A target invariant must not silently prove a candidate's preservation.
    let mut target_assumption = d.clone();
    target_assumption["components"]["Check"]["invariant"] = json!(["not", "s.a"]);
    let circular = run(
        "target-circular",
        &target_assumption,
        &json!([candidate("b", json!(["not", "s.b"]), vec![])]),
    )
    .unwrap();
    assert_eq!(circular["status"], "induction_counterexample");
    for (name, p) in [
        ("self", json!([candidate("a", json!(true), vec!["a"])])),
        (
            "cycle",
            json!([
                candidate("a", json!(true), vec!["b"]),
                candidate("b", json!(true), vec!["a"])
            ]),
        ),
        ("input", json!([candidate("a", json!("i.input"), vec![])])),
        ("future", json!([candidate("a", json!("n.a"), vec![])])),
        (
            "wrong-sort",
            json!([candidate("a", json!(["bv", 8, 0]), vec![])]),
        ),
        (
            "forged",
            json!([{"name":"a","predicate":true,"depends_on":[],"status":"established"}]),
        ),
        (
            "guard",
            json!([{"name":"a","predicate":false,"depends_on":[],"guard":false}]),
        ),
        (
            "duplicate",
            json!([
                candidate("a", json!(true), vec![]),
                candidate("a", json!(true), vec![])
            ]),
        ),
    ] {
        assert!(run(name, &d, &p).is_err(), "{name}");
    }
    // Old reports are descriptive, and cannot serve as new proof proposals.
    assert!(run("saved-report", &d, &good["candidates"]).is_err());
    let mut changed = d.clone();
    changed["implementation"]["next"]["a"] = json!("i.input");
    let stale = run("changed-source", &changed, &json!([a])).unwrap();
    assert_eq!(stale["status"], "induction_counterexample");
    assert_eq!(stale["candidates"][0]["preservation_checked"], false);
    // No candidate can hide a false original reset or original transition.
    changed = d.clone();
    changed["components"]["Check"]["init"] = json!("s.a");
    let bad_reset = run("target-reset", &changed, &json!([a])).unwrap();
    assert_eq!(bad_reset["status"], "induction_counterexample");
    assert_eq!(bad_reset["candidates"], json!([]));
    changed = d.clone();
    changed["implementation"]["next"]["b"] = json!(true);
    let bad_use = run("target-step", &changed, &json!([a])).unwrap();
    assert_eq!(bad_use["status"], "induction_counterexample");
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn induction_requires_finite_mode_without_external_fallback() {
    if std::env::var("LYDITE_INDUCTION_NONFINITE_CHILD").is_err() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "induction_requires_finite_mode_without_external_fallback",
                "--nocapture",
            ])
            .env("LYDITE_INDUCTION_NONFINITE_CHILD", "1")
            .env("LYDITE_SOLVER", "external-forbidden")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let out =
        std::env::temp_dir().join(format!("lydite-induction-nonfinite-{}", std::process::id()));
    let error = check_inductive_safety(
        &Specification::from_json(&document()).unwrap(),
        &json!([]),
        out.clone(),
    )
    .unwrap_err();
    assert!(error.contains("finite-only"));
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn budget_unknown_candidate_releases_no_induction_authority() {
    if std::env::var("LYDITE_INDUCTION_BUDGET_CHILD").is_err() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "budget_unknown_candidate_releases_no_induction_authority",
                "--nocapture",
            ])
            .env("LYDITE_INDUCTION_BUDGET_CHILD", "1")
            .env("LYDITE_SOLVER", "finite")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let mut d = document();
    d["components"]["Check"]["steps"]["tick"] = json!(true);
    // Fresh unconstrained words make this deliberately oversized proposal hit
    // the unchanged variable budget while encoding, not a timing threshold.
    // Reset folds the predicate to true; initialization must actually succeed.
    for i in 0..128 {
        let name = format!("word_{i}");
        d["inputs"][&name] = json!({"bv":64});
        d["implementation"]["state"][&name] = json!({"bv":64});
        d["implementation"]["reset"][&name] = json!(["bv", 64, 0]);
        d["implementation"]["next"][&name] = json!(format!("i.{name}"));
    }
    fn sum(terms: &[Value]) -> Value {
        if terms.len() == 1 {
            terms[0].clone()
        } else {
            let middle = terms.len() / 2;
            json!(["add", sum(&terms[..middle]), sum(&terms[middle..])])
        }
    }
    let terms = (0..64)
        .map(|i| {
            json!([
                "mul",
                format!("s.word_{}", 2 * i),
                format!("s.word_{}", 2 * i + 1)
            ])
        })
        .collect::<Vec<_>>();
    let proposals = json!([
        candidate(
            "oversized",
            json!(["eq", sum(&terms), ["bv", 64, 0]]),
            vec![]
        ),
        candidate("dependent", json!(true), vec!["oversized"])
    ]);
    let out = std::env::temp_dir().join(format!("lydite-induction-budget-{}", std::process::id()));
    let s = Specification::from_json(&d).unwrap();
    // A valid original target does not let a failed candidate masquerade as an
    // established invariant or permit the rest of this proposed proof to run.
    assert_eq!(
        check_inductive_safety(&s, &json!([]), out.join("bare")).unwrap()["status"],
        "inductive_safety_verified"
    );
    let r = check_inductive_safety(&s, &proposals, out.join("oversized")).unwrap();
    assert_eq!(r["status"], "unknown");
    assert_eq!(r["failed_stage"], "induction_candidate_0_preservation");
    assert_eq!(r["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(r["candidates"][0]["initialization_checked"], true);
    assert_eq!(r["candidates"][0]["preservation_checked"], false);
    assert_eq!(r["candidates"][0]["status"], "not_established");
    assert_eq!(r["target_uses"], json!([]));
    let failed = r["proofs"].as_array().unwrap().last().unwrap();
    assert_eq!(failed["status"], "unknown");
    assert!(failed["root"].is_null());
    assert_eq!(
        failed["children"][0]["finite"]["reason"],
        "finite solver variable budget exhausted"
    );
    assert_eq!(
        lydite_solver::finite::Limits::default().max_variables,
        200_000
    );
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn input_dependent_and_repeated_reset_preserve_induction() {
    if std::env::var("LYDITE_INDUCTION_RESET_CHILD").is_err() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "input_dependent_and_repeated_reset_preserve_induction",
                "--nocapture",
            ])
            .env("LYDITE_INDUCTION_RESET_CHILD", "1")
            .env("LYDITE_SOLVER", "finite")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let mut d = document();
    d["implementation"]["reset"] = json!({"a":"i.input","b":"i.input"});
    d["implementation"]["next"] = json!({"a":["xor","s.a","i.input"],"b":["xor","s.b","i.input"]});
    d["components"]["Check"]["init"] = json!(["eq", "s.a", "s.b"]);
    d["components"]["Check"]["steps"]["tick"] = json!(["eq", "n.a", "n.b"]);
    let s = Specification::from_json(&d).unwrap();
    let out = std::env::temp_dir().join(format!("lydite-induction-reset-{}", std::process::id()));
    let p = json!([candidate("equal", json!(["eq", "s.a", "s.b"]), vec![])]);
    let proved = check_inductive_safety(&s, &p, out.join("good")).unwrap();
    assert_eq!(proved["status"], "inductive_safety_verified");
    assert_eq!(proved["environment_assumptions"], json!([]));
    // Feasible reset inputs cannot substitute for universal initialization:
    // this predicate holds for reset/input=false but fails for input=true.
    let bad = check_inductive_safety(
        &s,
        &json!([candidate("only_false_input", json!(["not", "s.a"]), vec![])]),
        out.join("bad-candidate"),
    )
    .unwrap();
    assert_eq!(bad["status"], "induction_counterexample");
    assert_eq!(bad["candidates"][0]["initialization_checked"], false);
    assert_eq!(bad["target_uses"], json!([]));
    let input_symbol = s.inputs()["input"].0.op.strip_prefix('@').unwrap();
    assert_eq!(
        bad["proofs"].as_array().unwrap().last().unwrap()["children"][0]["finite"]["assignments"]
            [input_symbol]["value"],
        true
    );
    // Independently enumerate every six-edge reset/input pattern beginning in
    // reset (2,048 sequences), using the ORIGINAL typed reset/next expressions.
    // This does not relax the bounded replay format's single-reset restriction.
    use lydite_solver::finite::{Limits, Scalar, evaluate_scalar_terms};
    use std::collections::BTreeMap;
    let m = &s.implementation().unwrap().machine;
    let mut repeated = 0;
    for bits in 0..4096u32 {
        if bits & 1 == 0 {
            continue;
        }
        let (mut a, mut b) = (false, false);
        let mut resets = 0;
        for edge in 0..6 {
            let rst = bits & (1 << (2 * edge)) != 0;
            let input = bits & (1 << (2 * edge + 1)) != 0;
            let values = BTreeMap::from([
                (s.inputs()["rst"].0.op[1..].to_owned(), Scalar::Bool(rst)),
                (
                    s.inputs()["input"].0.op[1..].to_owned(),
                    Scalar::Bool(input),
                ),
                (m.state["a"].0.op[1..].to_owned(), Scalar::Bool(a)),
                (m.state["b"].0.op[1..].to_owned(), Scalar::Bool(b)),
            ]);
            let post = evaluate_scalar_terms(
                if rst { &m.reset } else { &m.next },
                &values,
                Limits::default(),
            )
            .unwrap();
            let expected = if rst { input } else { a ^ input };
            assert_eq!(post["a"], Scalar::Bool(expected));
            assert_eq!(post["b"], Scalar::Bool(expected));
            a = expected;
            b = expected;
            resets += usize::from(rst);
        }
        repeated += usize::from(resets > 1);
    }
    assert_eq!(repeated, 1984);
    // A defective reset on just one input valuation must fail the original
    // target reset before any strengthening is established.
    d["implementation"]["reset"]["b"] = json!(false);
    let r = check_inductive_safety(
        &Specification::from_json(&d).unwrap(),
        &p,
        out.join("bad-reset"),
    )
    .unwrap();
    assert_eq!(r["status"], "induction_counterexample");
    assert_eq!(r["failed_stage"], "reset_establishment");
    assert_eq!(r["candidates"], json!([]));
    fs::remove_dir_all(out).unwrap();
}
