use super::*;
use crate::{frontend::*, ir::*, solver::Check};
use std::collections::BTreeMap;
fn fixture(name: &str) -> Value {
    serde_json::from_slice(
        &fs::read(format!(
            "{}/../../hwverify/examples/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    )
    .unwrap()
}
fn z3() -> String {
    env::var("Z3_BIN").unwrap_or(format!(
        "{}/../../../proof-binding-study/.venv/bin/z3",
        env!("CARGO_MANIFEST_DIR")
    ))
}
fn output(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-evidence")
        .join(name);
    fs::create_dir_all(&p).unwrap();
    p
}
fn checked(name: &str) -> Value {
    check(&fixture(name), z3(), output(name)).unwrap()
}
#[test]
fn cpu_refines_isa() {
    assert_eq!(checked("cpu")["status"], "stuttering_refinement_verified");
}
#[test]
fn real_mutations_are_sat() {
    for n in [
        "wrong_load",
        "wrong_branch",
        "ignores_stall",
        "missing_commit",
        "hang_fetch",
        "commit_after_halt",
    ] {
        let r = checked(n);
        assert_eq!(r["status"], "counterexample", "{n}: {r}");
        assert!(
            r["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["status"] == "counterexample" && o["solver_result"] == "sat")
        );
    }
}
#[test]
fn input_rank_rejected() {
    assert!(
        check(&fixture("input_dependent_rank"), z3(), output("bad_rank"))
            .unwrap_err()
            .contains("unknown reference i.stall")
    );
}
#[test]
fn input_binding_rejected() {
    let mut d = fixture("cpu");
    d["binding"] = json!(["eq", "i.stall", "impl.halted"]);
    assert!(
        check(&d, z3(), output("bad_binding"))
            .unwrap_err()
            .contains("unknown reference i.stall")
    );
}
#[test]
fn bool_word_mix_rejected() {
    let mut l = Lower::default();
    assert!(
        l.expr(&json!(["and", true, ["bv", 1, 1]]), &Env::new())
            .is_err()
    );
}
#[test]
fn width_mismatch_rejected() {
    let mut l = Lower::default();
    assert!(
        l.expr(&json!(["add", ["bv", 3, 1], ["bv", 4, 1]]), &Env::new())
            .is_err()
    );
}
#[test]
fn literals_wrap() {
    let mut l = Lower::default();
    assert_eq!(l.expr(&json!(["bv", 3, 9]), &Env::new()).unwrap(), bv(3, 1));
    assert_eq!(
        l.expr(&json!(["bv", 3, -1]), &Env::new()).unwrap(),
        bv(3, 7)
    );
}
#[test]
fn unknown_operator_rejected() {
    let mut l = Lower::default();
    assert!(l.expr(&json!(["eval", "anything"]), &Env::new()).is_err());
}
#[test]
fn wire_cycle_rejected() {
    let mut d = fixture("cpu");
    d["impl"]["wires"]["bad"] = json!("w.bad");
    assert!(
        check(&d, z3(), output("wire_cycle"))
            .unwrap_err()
            .contains("wire cycle")
    );
}
#[test]
fn memory_normalization_is_equivalent() {
    for aw in [2, 8, 32] {
        let mut rules = BTreeMap::new();
        let m = var("m".into(), Sort::Mem(aw, 8));
        let a = var("a".into(), Sort::Bv(aw));
        let b = var("b".into(), Sort::Bv(aw));
        let v = var("v".into(), Sort::Bv(8));
        let w = var("w".into(), Sort::Bv(8));
        let first = node(Sort::Mem(aw, 8), "store", vec![m.clone(), a.clone(), v]);
        let second = node(
            Sort::Mem(aw, 8),
            "store",
            vec![first.clone(), b.clone(), w.clone()],
        );
        let raw = node(Sort::Bv(8), "select", vec![second.clone(), a.clone()]);
        let normalized = memory_read(second, a.clone(), 32, &mut rules);
        let overwritten = memory_write(first, a.clone(), w.clone(), &mut rules);
        let expected = node(Sort::Mem(aw, 8), "store", vec![m, a, w]);
        let mut q = Check {
            z3: z3(),
            out: output(&format!("memory_{aw}")),
            reports: vec![],
        };
        q.query("read_law", not(eq(raw, normalized)), false, &Env::new())
            .unwrap();
        q.query(
            "write_law",
            not(eq(overwritten, expected)),
            false,
            &Env::new(),
        )
        .unwrap();
        assert!(q.reports.iter().all(|r| r["status"] == "passed"));
        assert!(rules["read_after_write_alias_split"] > 0);
        assert!(rules["last_write_wins"] > 0);
    }
}

#[test]
fn duplicate_json_keys_rejected() {
    assert!(
        crate::json_input::parse(br#"{"version":2,"version":2}"#)
            .unwrap_err()
            .contains("duplicate JSON key")
    );
    assert!(crate::json_input::parse(br#"{"state":{"a":"bool","a":"bool"}}"#).is_err());
    assert!(crate::json_input::parse(br#"{"a":[true,3,null]}"#).is_ok());
}
#[cfg(unix)]
#[test]
fn solver_unknown_and_witness_recheck_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = output("solver_mock");
    let script = dir.join("mock-z3.sh");
    fs::write(
        &script,
        "#!/bin/sh\nx=$(cat)\ncase \"$x\" in *get-model*) echo unknown ;; *) echo sat ;; esac\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let mut q = Check {
        z3: script.to_str().unwrap().into(),
        out: dir.clone(),
        reports: vec![],
    };
    assert!(
        q.query("unstable_sat", boolv(true), false, &Env::new())
            .unwrap_err()
            .contains("witness recheck")
    );
    assert!(q.reports.is_empty());
    fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho unknown\n").unwrap();
    q.query("unknown", boolv(true), false, &Env::new()).unwrap();
    assert_eq!(q.reports[0]["status"], "unknown");
}

#[test]
fn array_sum_total_correctness_and_refinement() {
    let r = checked("array_sum");
    assert_eq!(r["status"], "program_and_refinement_verified", "{r}");
    assert!(
        r["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["status"] == "passed")
    );
}
#[test]
fn array_sum_negative_contracts_and_programs() {
    for (name, expected_obligation) in [
        ("array_sum_false_invariant", "program_initialization"),
        ("array_sum_bad_rank", "program_rank_decreases"),
        ("array_sum_bad_post", "program_postcondition"),
        (
            "array_sum_bad_program",
            "program_invariant_preserved_index_240_pc_1",
        ),
        ("array_sum_wrong_indexed_load", "microstep_refinement"),
        ("array_sum_missing_case", "program_cases_cover"),
    ] {
        let r = checked(name);
        assert_eq!(r["status"], "counterexample", "{name}: {r}");
        assert!(
            r["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["name"] == expected_obligation && o["status"] == "counterexample"),
            "{name}: {r}"
        );
    }
    assert_eq!(
        checked("array_sum_false_pre")["status"],
        "inadequate_contract"
    );
}
#[test]
fn array_sum_rejects_input_dependent_invariant_and_malformed_contract() {
    assert!(
        check(
            &fixture("array_sum_input_invariant"),
            z3(),
            output("program_input_inv")
        )
        .unwrap_err()
        .contains("unknown reference i.stall")
    );
    let mut d = fixture("array_sum");
    d["program_contract"]["rank"] = json!(true);
    assert!(
        check(&d, z3(), output("program_bool_rank"))
            .unwrap_err()
            .contains("program rank must be unsigned word")
    );
    d["program_contract"]["rank"] = json!(["bv", 10, 0]);
    d["program_contract"]["postcondition"] = json!("s.missing");
    assert!(
        check(&d, z3(), output("program_bad_post_ref"))
            .unwrap_err()
            .contains("unknown reference s.missing")
    );
}

#[test]
fn program_terminal_and_step_soundness() {
    for (name, obligation) in [
        ("array_sum_bad_terminal", "program_terminal_quiescent"),
        ("array_sum_unavailable_step", "program_step_available"),
        (
            "array_sum_split_other_bad_program",
            "program_invariant_preserved_index_other_pc_1",
        ),
    ] {
        let r = checked(name);
        assert_eq!(r["status"], "counterexample");
        assert!(
            r["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["name"] == obligation && o["status"] == "counterexample"),
            "{name}: {r}"
        );
    }
}
#[test]
fn program_invalid_split_and_input_rank_rejected() {
    let mut d = fixture("array_sum");
    d["program_contract"]["rank"] = json!("i.start_index");
    assert!(
        check(&d, z3(), output("program_input_rank"))
            .unwrap_err()
            .contains("unknown reference i.start_index")
    );
    let mut d = fixture("array_sum");
    d["program_contract"]["split"]["index"]["max"] = json!(256);
    assert!(
        check(&d, z3(), output("program_bad_split_width"))
            .unwrap_err()
            .contains("split bounds invalid")
    );
}

#[test]
fn program_precondition_must_allow_actual_reset() {
    let mut d = fixture("array_sum");
    d["program_contract"]["precondition"] = json!([
        "and",
        d["program_contract"]["precondition"].clone(),
        ["not", "i.rst"]
    ]);
    let r = check(&d, z3(), output("program_impossible_reset")).unwrap();
    assert_eq!(r["status"], "inadequate_contract");
    assert!(
        r["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "program_pre_nonempty" && o["status"] == "failed_nonvacuity")
    );
}

#[test]
fn automatic_partitions_keep_complements_and_wrapped_literals() {
    let s = symbols(&json!({"x":{"bv":4},"wide":{"bv":64}}), "spec").unwrap();
    let mut l = Lower::default();
    // Disjunctive, negated, signed and modular values are hints, never bounds.
    let inv = l
        .expr(
            &json!([
                "or",
                ["not", ["ule", "s.x", ["bv", 4, 2]]],
                [
                    "and",
                    ["slt", ["bv", 4, 17], "s.x"],
                    ["eq", "s.wide", ["bv", 64, 18446744073709551615u64]]
                ]
            ]),
            &scope(&s, &Env::new()),
        )
        .unwrap();
    let (parts, plan) = crate::program::infer_partitions(&s, &inv, &Env::new(), &inv);
    assert!(plan["partition_count"].as_u64().unwrap() <= 128);
    let mut covered = boolv(false);
    for (_, guard) in parts.unwrap_or(vec![(String::new(), boolv(true))]) {
        covered = node(Sort::Bool, "or", vec![covered, guard]);
    }
    let mut q = Check {
        z3: z3(),
        out: output("auto_coverage"),
        reports: vec![],
    };
    // Coverage without invariant assumptions is stronger than active-state cover.
    q.query("all_states_covered", not(covered), false, &Env::new())
        .unwrap();
    assert_eq!(q.reports[0]["status"], "passed");
    assert!(
        plan["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["state"] == "x" && c["values"] == json!([1, 2]))
    );
}

#[test]
fn automatic_planning_is_order_independent_and_bounded() {
    let s = symbols(
        &json!({"x":{"bv":8},"y":{"bv":8},"z":{"bv":8},"w":{"bv":8}}),
        "spec",
    )
    .unwrap();
    let mut clauses = vec![];
    for name in ["x", "y", "z", "w"] {
        for v in 0..16 {
            clauses.push(eq(s[name].clone(), bv(8, v)));
        }
    }
    let left = clauses.iter().cloned().fold(boolv(true), and);
    let right = clauses
        .into_iter()
        .rev()
        .map(|t| eq(t.0.args[1].clone(), t.0.args[0].clone()))
        .fold(boolv(true), and);
    let (_, p) = crate::program::infer_partitions(&s, &left, &Env::new(), &left);
    let (_, p2) = crate::program::infer_partitions(&s, &right, &Env::new(), &right);
    let mut p = p;
    let mut p2 = p2;
    p.as_object_mut().unwrap().remove("scoring_seconds");
    p2.as_object_mut().unwrap().remove("scoring_seconds");
    assert_eq!(p, p2);
    assert!(p["partition_count"].as_u64().unwrap() <= 128);
    assert!(
        p["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["decision"] == "low_marginal_benefit")
    );
    let many = (0..33)
        .map(|v| eq(s["x"].clone(), bv(8, v)))
        .fold(boolv(true), and);
    let (parts, p) = crate::program::infer_partitions(&s, &many, &Env::new(), &many);
    assert!(parts.is_none());
    assert_eq!(p["candidates"][0]["decision"], "too_many_values");
}

#[test]
fn extension_amount_overflow_is_rejected() {
    let mut l = Lower::default();
    for op in ["zext", "sext"] {
        assert!(
            l.expr(
                &json!([op, 18446744073709551615u64, ["bv", 8, 0]]),
                &Env::new()
            )
            .is_err()
        );
    }
}

#[test]
fn automatic_array_sum_and_empty_cases_fail_closed() {
    assert_eq!(
        checked("auto_array_sum")["status"],
        "program_and_refinement_verified"
    );
    let mut d = fixture("auto_array_sum");
    d["program_contract"]["cases"] = json!({});
    let r = check(&d, z3(), output("auto_empty_cases")).unwrap();
    assert_eq!(r["status"], "counterexample");
    assert!(
        r["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "program_cases_cover" && o["status"] == "counterexample")
    );
    let mut d = fixture("auto_array_sum");
    d["program_contract"] = json!({});
    assert!(
        check(&d, z3(), output("auto_empty_contract"))
            .unwrap_err()
            .contains("missing field")
    );
}

#[cfg(unix)]
#[test]
fn automatic_unknown_never_verifies() {
    use std::os::unix::fs::PermissionsExt;
    let dir = output("auto_unknown");
    let script = dir.join("z3.sh");
    fs::write(&script, "#!/bin/sh\ncat >/dev/null\necho unknown\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let r = check(
        &fixture("auto_array_sum"),
        script.to_str().unwrap().into(),
        dir,
    )
    .unwrap();
    assert_eq!(r["status"], "unknown");
    assert!(
        r["obligations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["status"] == "unknown"
                || (o["status"] == "passed" && o["backend"] == "structural_kernel"))
    );
}

#[test]
fn exhausted_partition_budget_records_unproved_obligation() {
    let mut q = Check {
        z3: "must-not-run".into(),
        out: output("auto_budget"),
        reports: vec![],
    };
    crate::program::budgeted_query(
        &mut q,
        "skipped",
        boolv(true),
        &Env::new(),
        std::time::Instant::now(),
        0,
    )
    .unwrap();
    assert_eq!(q.reports[0]["status"], "unknown");
    assert_eq!(q.reports[0]["solver_result"], "not_run");
}

#[cfg(unix)]
#[test]
fn every_invalid_json_field_is_validated_before_starting_a_solver() {
    use std::os::unix::fs::PermissionsExt;
    let dir = output("preflight_validation");
    let marker = dir.join("solver-was-started");
    if marker.exists() {
        fs::remove_file(&marker).unwrap();
    }
    let script = dir.join("sentinel-z3.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat >/dev/null\necho unknown\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let solver = script.to_str().unwrap().to_string();
    let mut d = fixture("array_sum");
    d["program_contract"]["postcondition"] = json!("s.unknown_late_field");
    assert!(
        check(&d, solver.clone(), dir.clone())
            .unwrap_err()
            .contains("unknown reference")
    );
    d["program_contract"]["postcondition"] = json!(true);
    d["program_contract"]["split"]["index"]["max"] = json!(256);
    assert!(
        check(&d, solver, dir)
            .unwrap_err()
            .contains("split bounds invalid")
    );
    assert!(
        !marker.exists(),
        "invalid model launched the external solver"
    );
}
