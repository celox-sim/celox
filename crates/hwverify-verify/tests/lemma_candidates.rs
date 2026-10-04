use hwverify_ir::*;
use hwverify_solver::Check;
use hwverify_verify::{lemma_candidate::LemmaCandidate, proof_program::ProofPrograms};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, process::Command};
fn candidate(id: &str, guard: Value, claim: Value) -> Value {
    LemmaCandidate {
        context: json!(true),
        guard,
        claim,
        depends_on: vec![],
        source: Some(format!("example/{id}")),
    }
    .to_step(id)
}
fn metadata(steps: Vec<Value>, result: &str, mode: &str) -> Value {
    json!({"version":1,"mode":mode,"variables":{},"lets":[],"programs":[{"id":"example","match_rhs":["bv",8,0],"steps":steps,"result":result}]})
}
fn context() -> Env {
    BTreeMap::from([
        ("impl.x".into(), var("old".into(), Sort::Bv(8))),
        ("impl_next.x".into(), var("next".into(), Sort::Bv(8))),
        ("i.g".into(), var("guard".into(), Sort::Bool)),
    ])
}
#[test]
fn human_and_programmatic_data_have_one_typed_representation() {
    let generated = candidate("c", json!("i.g"), json!(["eq", "impl.x", "impl.x"]));
    let hand = json!({"op":"candidate","id":"c","frame":"current_query","context":true,"guard":"i.g","claim":["eq","impl.x","impl.x"],"depends_on":[],"source":"example/c"});
    assert_eq!(generated, hand);
    let a = LemmaCandidate::from_step(&hand).unwrap();
    assert_eq!(a.to_step("c"), generated);
    assert_eq!(a.lower(&context()).unwrap().claim().0.sort, Sort::Bool);
}
#[test]
fn malformed_width_frame_cycles_and_uncovered_cases_reject() {
    let mut c = candidate("c", json!(true), json!(["eq", "impl.x", ["bv", 16, 0]]));
    assert!(ProofPrograms::from_json(
        &metadata(vec![c.clone()], "c", "independent_lemmas"),
        &context()
    )
    .is_err());
    c["claim"] = json!(true);
    c["frame"] = json!("next_cycle_induction");
    assert!(ProofPrograms::from_json(
        &metadata(vec![c.clone()], "c", "independent_lemmas"),
        &context()
    )
    .is_err());
    c["frame"] = json!("current_query");
    c["depends_on"] = json!(["c"]);
    let err = ProofPrograms::from_json(
        &metadata(vec![c.clone()], "c", "independent_lemmas"),
        &context(),
    )
    .err()
    .unwrap();
    assert!(err.contains("cycle") && err.contains("/steps/0"));
    let mut d = candidate("d", json!(true), json!(true));
    c["depends_on"] = json!(["d"]);
    d["depends_on"] = json!(["c"]);
    let err =
        ProofPrograms::from_json(&metadata(vec![c, d], "d", "independent_lemmas"), &context())
            .err()
            .unwrap();
    assert!(err.contains("(c)") && err.contains("(d)") && err.contains("cycle"));
    let missing =
        json!({"op":"join","id":"j","pre":"$pre","goal":"$goal","guard":"i.g","positive":"c"});
    assert!(ProofPrograms::from_json(
        &metadata(
            vec![candidate("c", json!(true), json!(true)), missing],
            "j",
            "independent_lemmas"
        ),
        &context()
    )
    .is_err());
    let mut forged = candidate("c", json!(true), json!(true));
    forged["checked"] = json!(true);
    assert!(ProofPrograms::from_json(
        &metadata(vec![forged], "c", "independent_lemmas"),
        &context()
    )
    .is_err());
}
#[test]
fn live_candidate_boundaries() {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "live_worker", "--nocapture"])
        .env("HWVERIFY_SOLVER", "finite")
        .env("CANDIDATE_WORKER", "1")
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn live_worker() {
    if std::env::var("CANDIDATE_WORKER").as_deref() != Ok("1") {
        return;
    }
    let root = std::env::temp_dir().join(format!("lemma-candidates-{}", std::process::id()));
    let ctx = context();
    let goal = eq(ctx["impl.x"].clone(), bv(8, 0));
    let run = |name: &str, data: Value, bad: Term| {
        let out = root.join(name);
        fs::create_dir_all(&out).unwrap();
        let mut check = Check {
            out,
            z3: "FORBIDDEN_EXTERNAL_SOLVER".into(),
            reports: vec![],
        };
        let p = ProofPrograms::from_json(&data, &ctx).unwrap();
        let result = p.try_query(&mut check, name, &bad, &ctx);
        (result, check.reports)
    };
    for mode in ["independent_lemmas", "shared_query"] {
        let c = candidate(
            "c",
            json!(["eq", "impl.x", ["bv", 8, 0]]),
            json!(["eq", "impl.x", ["bv", 8, 0]]),
        );
        let use_step = json!({"op":"use_candidate","id":"done","candidate":"c","context":"$pre"});
        let (r, reports) = run(
            mode,
            metadata(vec![c, use_step], "done", mode),
            and(goal.clone(), not(goal.clone())),
        );
        assert!(r.unwrap());
        let d = &reports[0]["lemma_candidates"];
        assert_eq!(d["target_closed"], true);
        assert_eq!(d["candidates"][0]["state"], "applied");
        assert_eq!(d["uses"][0]["state"], "applied");
    }
    for (name, guard) in [
        ("missing_guard", json!(["eq", "impl.x", ["bv", 8, 0]])),
        ("false_guard", json!(false)),
    ] {
        let c = candidate("c", guard, json!(["eq", "impl.x", ["bv", 8, 0]]));
        let use_step = json!({"op":"use_candidate","id":"done","candidate":"c","context":"$pre"});
        let (r, reports) = run(
            name,
            metadata(vec![c, use_step], "done", "independent_lemmas"),
            and(boolv(true), not(goal.clone())),
        );
        assert!(r.is_err());
        let d = &reports[0]["lemma_candidates"];
        assert_eq!(d["candidates"][0]["validity"], "established");
        assert_eq!(
            d["uses"][0]["state"],
            "use_context_does_not_establish_guard"
        );
        assert_eq!(d["target_closed"], false);
        assert_eq!(d["candidates"][0]["reachable_from_reset"], "not_checked");
    }
    let c = candidate("c", json!(true), json!(["eq", "impl.x", ["bv", 8, 0]]));
    let mut dependent = candidate("d", json!(true), json!(true));
    dependent["depends_on"] = json!(["c"]);
    let (r, reports) = run(
        "false",
        metadata(vec![c, dependent], "d", "independent_lemmas"),
        and(boolv(true), not(goal.clone())),
    );
    assert!(r.is_err());
    let d = &reports[0]["lemma_candidates"];
    assert_eq!(d["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(d["candidates"][0]["validity"], "lemma_counterexample");
    assert!(d["candidates"][0]["claim_true"].is_null());
    assert_eq!(
        reports[0]["original_recheck"]["finite"]["original_formula_validated"],
        true
    );
    let unused = candidate("unused", json!(true), json!(true));
    let done = candidate("done", json!("$pre"), json!("$goal"));
    let (_, reports) = run(
        "unused",
        metadata(
            vec![
                unused,
                json!({"op":"use_candidate","id":"dead_use","candidate":"unused","context":"$pre"}),
                done,
            ],
            "done",
            "independent_lemmas",
        ),
        and(goal.clone(), not(goal.clone())),
    );
    assert_eq!(
        reports[0]["lemma_candidates"]["candidates"][0]["usefulness"],
        "established_but_unused"
    );
    let wrong = candidate(
        "c",
        json!(["eq", "impl.x", ["bv", 8, 0]]),
        json!(["eq", "impl.x", ["bv", 8, 0]]),
    );
    let (r, reports) = run(
        "wrong_frame",
        metadata(vec![wrong], "c", "independent_lemmas"),
        and(goal.clone(), not(eq(ctx["impl_next.x"].clone(), bv(8, 0)))),
    );
    assert!(r.is_err());
    assert_eq!(reports[0]["lemma_candidates"]["target_closed"], false);
    let p = ProofPrograms::from_json(
        &metadata(
            vec![candidate("c", json!("$pre"), json!("$goal"))],
            "c",
            "independent_lemmas",
        ),
        &ctx,
    )
    .unwrap();
    let mut stale = ctx.clone();
    stale.insert(
        "impl_next.x".into(),
        var("changed_model".into(), Sort::Bv(8)),
    );
    let mut check = Check {
        out: root.join("unused_path"),
        z3: "FORBIDDEN".into(),
        reports: vec![],
    };
    assert!(p
        .try_query(&mut check, "stale", &and(goal.clone(), not(goal)), &stale)
        .is_err());
    assert!(check.reports.is_empty());
    // A difficult valid arithmetic claim must not release a dependent handle on Unknown.
    let mut d = metadata(
        vec![
            candidate(
                "hard",
                json!(true),
                json!([
                    "eq",
                    ["mul", "formal.a", ["add", "formal.b", "formal.c"]],
                    [
                        "add",
                        ["mul", "formal.a", "formal.b"],
                        ["mul", "formal.a", "formal.c"]
                    ]
                ]),
            ),
            candidate("after", json!(true), json!(true)),
        ],
        "after",
        "shared_query",
    );
    d["variables"] = json!({"a":{"bv":64},"b":{"bv":64},"c":{"bv":64}});
    d["programs"][0]["steps"][1]["depends_on"] = json!(["hard"]);
    let (r, reports) = run(
        "unknown",
        d,
        and(boolv(true), not(eq(ctx["impl.x"].clone(), bv(8, 0)))),
    );
    assert!(r.is_err());
    let d = &reports[0]["lemma_candidates"];
    assert_eq!(d["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(d["candidates"][0]["validity"], "unknown_budget");
    assert!(d["candidates"][0]["claim_true"].is_null());
    // Human labels may collide; attribution must use the live query position.
    for fail in [false, true] {
        let seed = candidate("seed", json!(true), json!(true));
        let usage = json!({"op":"use_candidate","id":"u","candidate":"seed","context":"$pre","source":"collision.hwv:3:5"});
        let claim = if fail {
            json!(["eq", "impl.x", ["bv", 8, 0]])
        } else {
            json!("$goal")
        };
        let last = candidate("u_guard_at_use", json!("$pre"), claim);
        let bad = if fail {
            and(boolv(true), not(eq(ctx["impl.x"].clone(), bv(8, 0))))
        } else {
            let g = eq(ctx["impl.x"].clone(), bv(8, 0));
            and(g.clone(), not(g))
        };
        let (result, reports) = run(
            if fail {
                "collision_fail"
            } else {
                "collision_ok"
            },
            metadata(
                vec![seed, usage, last],
                "u_guard_at_use",
                "independent_lemmas",
            ),
            bad,
        );
        let d = &reports[0]["lemma_candidates"];
        assert_eq!(d["uses"][0]["source"], "collision.hwv:3:5");
        assert_ne!(
            d["uses"][0]["query_index"],
            d["candidates"][1]["query_index"]
        );
        if fail {
            assert!(result.is_err());
            assert_eq!(d["candidates"][1]["validity"], "lemma_counterexample");
        } else {
            assert!(result.unwrap());
            assert_eq!(d["candidates"][1]["usefulness"], "target_closed");
            assert_eq!(d["candidates"][1]["proof_node"], reports[0]["root"]);
        }
    }
    // A legacy prove label must not hide a later failed guard query.
    let c = candidate("guarded", json!(false), json!(true));
    let legacy = json!({"op":"prove","id":"u_guard_at_use","pre":true,"post":true});
    let usage = json!({"op":"use_candidate","id":"u","candidate":"guarded","context":"$pre","source":"guard.hwv:9:5"});
    let (r, reports) = run(
        "legacy_collision",
        metadata(vec![c, legacy, usage], "u", "independent_lemmas"),
        and(boolv(true), not(eq(ctx["impl.x"].clone(), bv(8, 0)))),
    );
    assert!(r.is_err());
    assert_eq!(
        reports[0]["lemma_candidates"]["uses"][0]["state"],
        "use_context_does_not_establish_guard"
    );
    // Replacing a model alias with an identically named formal must not pass identity checks.
    let original = Env::from([("impl.x".into(), ctx["impl.x"].clone())]);
    let mut doc = metadata(
        vec![candidate("c", json!("$pre"), json!("$goal"))],
        "c",
        "independent_lemmas",
    );
    doc["variables"] = json!({"a":{"bv":8}});
    let p = ProofPrograms::from_json(&doc, &original).unwrap();
    let substituted = Env::from([(
        "formal.a".into(),
        var("proof_program_formal_a".into(), Sort::Bv(8)),
    )]);
    let mut check = Check {
        out: root.join("context_collision"),
        z3: "FORBIDDEN".into(),
        reports: vec![],
    };
    let g = eq(ctx["impl.x"].clone(), bv(8, 0));
    assert!(p
        .try_query(
            &mut check,
            "identity",
            &and(g.clone(), not(g)),
            &substituted
        )
        .unwrap_err()
        .contains("stale proof-program context"));
    assert!(check.reports.is_empty());
    fs::remove_dir_all(root).unwrap();
}
