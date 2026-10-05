use lydite_ir::{ScopedSpecification, Specification};
use lydite_verify::{check_scoped_specification, check_specification};
use serde_json::{Value, json};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lydite")
}
fn z3() -> String {
    std::env::var("Z3_BIN").unwrap_or_else(|_| {
        root()
            .join("../proof-binding-study/.venv/bin/z3")
            .display()
            .to_string()
    })
}
fn example(mode: &str, quantifiers: Value, ensure: Value) -> Value {
    json!({"expect":mode,"quantifiers":quantifiers,"initial":{},"trace":[{"operation":"tick","inputs":{"x":"q.a"},"observe":{},"ensure":ensure}]})
}
fn doc(relation: Value, examples: Value) -> Value {
    json!({"version":3,"kind":"specification","inputs":{"x":{"bv":2}},"observations":{"y":{"bv":2}},"operations":{"tick":{}},"components":{"C":{"state":{},"init":true,"invariant":true,"steps":{"tick":relation},"examples":examples}},"compositions":{}})
}
fn binder(kind: &str, name: &str) -> Value {
    json!({"kind":kind,"variables":{name:{"bv":2}}})
}
fn check(document: &Value, name: &str) -> Value {
    check_specification(
        &Specification::from_json(document).unwrap(),
        z3(),
        root().join("../target/quantified-regressions").join(name),
    )
    .unwrap()
}
fn case<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["examples"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["example"] == name)
        .unwrap()
}

#[test]
fn ordered_input_binders_change_truth_and_have_no_global_bound_declarations() {
    let ensure = json!(["eq", "o.y", "q.b"]);
    let report = check(
        &doc(
            json!(["eq", "no.y", "i.x"]),
            json!({
                "dependent":example("exists",json!([binder("forall","a"),binder("exists","b")]),ensure.clone()),
                "constant":example("exists",json!([binder("exists","b"),binder("forall","a")]),ensure)
            }),
        ),
        "order",
    );
    assert_eq!(case(&report, "dependent")["valid"], true);
    assert_eq!(case(&report, "constant")["valid"], false);
    let evidence = &case(&report, "dependent")["evidence"];
    assert_eq!(evidence["backend"], "z3_quantified");
    assert_eq!(evidence["kernel"]["enabled"], false);
    let script = std::fs::read_to_string(
        root()
            .join("../target/quantified-regressions/order")
            .join(evidence["evidence"].as_str().unwrap()),
    )
    .unwrap();
    assert!(!script.contains("(declare-fun"));
    assert!(!script.contains("(define-fun"));
    assert!(script.contains("(forall"));
    assert!(script.contains("(exists"));
    assert!(script.contains("(let"));
}

#[test]
fn universal_outputs_are_never_assumed_and_dead_traces_fail_nonvacuity() {
    let prefix = json!([binder("forall", "a")]);
    let cases = json!({"exists":example("exists",prefix.clone(),json!(["eq","o.y","q.a"])),"forall":example("forall",prefix.clone(),json!(["eq","o.y","q.a"])),"exclude":example("not_exists",prefix,json!(["eq","o.y","q.a"]))});
    let report = check(&doc(json!(true), cases.clone()), "choice");
    assert_eq!(case(&report, "exists")["valid"], true);
    assert_eq!(case(&report, "forall")["valid"], false);
    assert_eq!(case(&report, "exclude")["valid"], false);
    let dead = check(&doc(json!(false), cases), "dead");
    assert_eq!(case(&dead, "exists")["valid"], false);
    assert_eq!(case(&dead, "forall")["valid"], false);
    assert_eq!(case(&dead, "exclude")["valid"], true);
    assert_eq!(case(&dead, "exclude")["feasibility"]["holds"], false);
    assert_eq!(case(&dead, "exclude")["vacuity"], "vacuous_exclusion");
}

#[test]
fn feasibility_and_postcondition_share_existential_input_choices() {
    // x=0 is feasible and satisfies y=0; x!=0 is infeasible. Separately,
    // existential feasibility and exclusion both hold, but never together.
    let relation = json!([
        "and",
        ["eq", "i.x", ["bv", 2, 0]],
        ["eq", "no.y", ["bv", 2, 0]]
    ]);
    let prefix = json!([binder("exists", "a")]);
    let report = check(
        &doc(
            relation,
            json!({
                "forall":example("forall",prefix.clone(),json!(["eq","o.y",["bv",2,1]])),
                "exclude":example("not_exists",prefix,json!(["eq","o.y",["bv",2,0]]))
            }),
        ),
        "correlation",
    );
    assert_eq!(case(&report, "forall")["valid"], false);
    assert_eq!(case(&report, "forall")["feasibility"]["holds"], true);
    assert_eq!(case(&report, "exclude")["valid"], true);
    assert_eq!(case(&report, "exclude")["feasibility"]["holds"], true);
    assert_eq!(case(&report, "exclude")["nonvacuous"]["holds"], false);
}

#[test]
fn earlier_expected_results_do_not_filter_later_universal_executions() {
    let mut e = example("forall", json!([binder("forall", "a")]), json!(true));
    e["trace"] = json!([
        {"operation":"tick","inputs":{},"observe":{"y":["bv",2,0]}},
        {"operation":"tick","inputs":{},"observe":{},"ensure":true}
    ]);
    let report = check(&doc(json!(true), json!({"two_steps":e})), "two_steps");
    assert_eq!(case(&report, "two_steps")["valid"], false);
    assert_eq!(case(&report, "two_steps")["feasibility"]["holds"], true);
}

#[test]
fn scoped_outputs_and_action_sets_use_quantified_path() {
    let document = json!({"version":4,"kind":"specification","specs":{"Echo":{
        "inputs":{"x":{"bv":2}},"outputs":{"y":{"bv":2}},"state":{},"init":true,"invariant":true,
        "operations":{"tick":["eq","no.y","x"]},
        "examples":{"all":{"expect":"forall","quantifiers":[binder("forall","a")],"initial":{},"trace":[{"actions":["tick"],"inputs":{"x":"q.a"},"observe":{},"ensure":["eq","y","q.a"]}]}}
    }},"compositions":{}});
    let report = check_scoped_specification(
        &ScopedSpecification::from_json(&document).unwrap(),
        z3(),
        root().join("../target/quantified-regressions/scoped"),
    )
    .unwrap();
    assert_eq!(case(&report, "all")["valid"], true);
    assert!(
        case(&report, "all")["feasibility"]["evidence"]["evidence"]
            .as_str()
            .unwrap()
            .starts_with("target_0000/")
    );
}

#[test]
fn bound_scope_sort_shadowing_and_execution_interleaving_are_rejected() {
    let base = doc(
        json!(true),
        json!({"e":example("forall",json!([binder("forall","a")]),json!(["eq","o.y","q.a"]))}),
    );
    for (field, value) in [
        ("ensure", json!(["eq", "o.y", "q.missing"])),
        ("ensure", json!(["eq", "o.y", "s.secret"])),
        ("ensure", json!(["eq", "o.y", "i.x"])),
        ("ensure", json!(["eq", "o.y", "no.y"])),
    ] {
        let mut bad = base.clone();
        bad["components"]["C"]["examples"]["e"]["trace"][0][field] = value;
        assert!(Specification::from_json(&bad).is_err());
    }
    let mut bad = base.clone();
    bad["components"]["C"]["examples"]["e"]["quantifiers"] =
        json!([binder("forall", "a"), binder("exists", "a")]);
    assert!(
        Specification::from_json(&bad)
            .unwrap_err()
            .message
            .contains("shadowing")
    );
    bad["components"]["C"]["examples"]["e"]["quantifiers"] =
        json!([{"kind":"execution","variables":{"a":"bool"}}]);
    assert!(Specification::from_json(&bad).is_err());
    bad["components"]["C"]["examples"]["e"]["quantifiers"] =
        json!([{"kind":"forall","variables":{"a":"bool"}}]);
    assert!(Specification::from_json(&bad).is_err());
}

#[test]
fn typed_boolean_and_array_binders_preserve_hygienic_names() {
    let document = json!({"version":3,"kind":"specification",
        "inputs":{"flag":"bool","memory":{"mem":[2,2]}},
        "observations":{"flag":"bool","memory":{"mem":[2,2]}},"operations":{"tick":{}},
        "components":{"C":{"state":{"quantified_let_0":"bool"},"init":true,"invariant":true,
            "steps":{"tick":["and",["eq","no.flag","i.flag"],["eq","no.memory","i.memory"]]},
            "examples":{"all":{"expect":"forall","quantifiers":[{"kind":"forall","variables":{"quantified_let_0":"bool","trace_obs_t1_v0":{"mem":[2,2]}}}],"initial":{},"trace":[{"operation":"tick","inputs":{"flag":"q.quantified_let_0","memory":"q.trace_obs_t1_v0"},"observe":{"flag":"q.quantified_let_0","memory":"q.trace_obs_t1_v0"}}]}}
        }},"compositions":{}});
    let report = check(&document, "typed_hygiene");
    assert_eq!(case(&report, "all")["valid"], true);
    assert_eq!(case(&report, "all")["nonvacuous"]["holds"], true);
}

#[test]
fn postconditions_support_arbitrary_boolean_disjunctions() {
    let relation = json!(["or", ["eq", "no.y", "i.x"], ["eq", "no.y", ["bv", 2, 0]]]);
    let ensure = json!(["or", ["eq", "o.y", "q.a"], ["eq", "o.y", ["bv", 2, 0]]]);
    let report = check(
        &doc(
            relation,
            json!({"all":example("forall",json!([binder("forall","a")]),ensure)}),
        ),
        "disjunction",
    );
    assert_eq!(case(&report, "all")["valid"], true);
}

#[cfg(unix)]
#[test]
fn unknown_backend_is_never_reported_as_a_pass() {
    use std::os::unix::fs::PermissionsExt;
    let out = root().join("../target/quantified-regressions/unknown");
    std::fs::create_dir_all(&out).unwrap();
    let fake = out.join("unknown-solver.sh");
    std::fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nprintf 'unknown\\n'\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    let document = doc(
        json!(true),
        json!({"e":example("forall",json!([binder("forall","a")]),json!(true))}),
    );
    let report = check_specification(
        &Specification::from_json(&document).unwrap(),
        fake.display().to_string(),
        out,
    )
    .unwrap();
    assert_eq!(case(&report, "e")["valid"], Value::Null);
    assert_eq!(case(&report, "e")["status"], "unknown");
    assert_eq!(case(&report, "e")["feasibility"]["holds"], Value::Null);
    assert_eq!(report["status"], "unknown");
}

#[test]
fn infeasibility_diagnostics_do_not_invent_a_complete_execution() {
    let report = check(
        &doc(
            json!(false),
            json!({"dead":{"expect":"forall","initial":{},"trace":[{"operation":"tick","inputs":{},"observe":{}}]}}),
        ),
        "no_execution_witness",
    );
    let entry = case(&report, "dead");
    assert_eq!(entry["valid"], false);
    assert_eq!(entry["concrete_witness"], Value::Null);
    assert_eq!(entry["witness_diagnostics"][0]["found"], false);
    assert_eq!(
        entry["witness_diagnostics"][1]["purpose"],
        "input_without_feasible_execution"
    );
    assert_eq!(entry["witness_diagnostics"][1]["found"], true);
}

#[cfg(unix)]
#[test]
fn unknown_witness_is_unavailable_and_changed_sat_recheck_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    let document = doc(
        json!(true),
        json!({"e":example("exists",json!([binder("exists","a")]),json!(true))}),
    );
    for (name, script) in [
        (
            "unknown_witness",
            "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *declare-fun*) printf 'unknown\\n';; *) printf 'sat\\n';; esac\n",
        ),
        (
            "changed_witness",
            "#!/bin/sh\ninput=$(cat)\ncase \"$input\" in *get-model*) printf 'unknown\\n';; *) printf 'sat\\n';; esac\n",
        ),
    ] {
        let out = root().join("../target/quantified-regressions").join(name);
        std::fs::create_dir_all(&out).unwrap();
        let fake = out.join("solver.sh");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let report = check_specification(
            &Specification::from_json(&document).unwrap(),
            fake.display().to_string(),
            out,
        );
        if name == "changed_witness" {
            assert!(report.unwrap_err().contains("no SAT result certified"));
        } else {
            let report = report.unwrap();
            let entry = case(&report, "e");
            assert_eq!(entry["concrete_witness"], Value::Null);
            assert_eq!(entry["witness_diagnostics"][0]["found"], Value::Null);
        }
    }
}
