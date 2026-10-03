use hwverify_ir::*;
use hwverify_solver::Check;
use hwverify_syntax::{merge_lemma_source, parse_document};
use hwverify_verify::proof_program::ProofPrograms;
use serde_json::json;
use std::{fs, process::Command};
const SOURCE: &str = r#"lemmas "native" {
  forall word: bv<8>;
  target zero {
    rhs 0u8;
    lemma c { context true; guard impl.x == 0u8; claim impl.x == 0u8; }
    use done: c(context: pre);
    result done;
  }
}"#;
fn context() -> Env {
    Env::from([
        ("impl.x".into(), var("x".into(), Sort::Bv(8))),
        ("impl_next.x".into(), var("next_x".into(), Sort::Bv(8))),
    ])
}
#[test]
fn native_syntax_has_one_canonical_api_and_direct_binders() {
    let d = merge_lemma_source(SOURCE, "native.hwv", None).unwrap();
    let s = &d["programs"][0]["steps"];
    assert_eq!(s[0]["op"], "candidate");
    assert_eq!(s[0]["claim"], json!(["eq", "impl.x", ["bv", 8, 0]]));
    assert_eq!(s[1]["op"], "use_candidate");
    assert_eq!(d["variables"]["word"], json!({"bv":8}));
    ProofPrograms::from_json(&d, &context()).unwrap();
    let source = SOURCE.replace("claim impl.x == 0u8;", "claim word == word;");
    let d = merge_lemma_source(&source, "binder.hwv", None).unwrap();
    assert_eq!(
        d["programs"][0]["steps"][0]["claim"],
        json!(["eq", "formal.word", "formal.word"])
    );
    let source = SOURCE.replace("claim impl.x == 0u8;", "claim impl.x' == impl.x';");
    let d = merge_lemma_source(&source, "prime.hwv", None).unwrap();
    assert_eq!(
        d["programs"][0]["steps"][0]["claim"],
        json!(["eq", "impl_next.x", "impl_next.x"])
    );
    assert!(merge_lemma_source(
        &SOURCE.replace("claim impl.x == 0u8;", "claim word' == word;"),
        "bad-prime.hwv",
        None
    )
    .is_err());
    let inline = SOURCE.replacen("lemmas \"native\" {", "design \"inline\" { proof {", 1) + "}";
    let d = parse_document(&inline, "inline.hwv").unwrap();
    assert!(d.canonical["proof_programs"]["programs"].is_array());
}
#[test]
fn cycle_width_and_module_boundary_diagnostics_are_source_linked() {
    let source = SOURCE.replace("claim impl.x == 0u8;", "claim impl.x == 0u16;");
    let d = merge_lemma_source(&source, "wrong-width.hwv", None).unwrap();
    let err = ProofPrograms::from_json(&d, &context()).err().unwrap();
    assert!(err.contains("wrong-width.hwv:5:"));
    let source = SOURCE.replace("claim impl.x == 0u8;", "claim impl.x == 0u8; depends done;");
    let d = merge_lemma_source(&source, "cycle.hwv", None).unwrap();
    let err = ProofPrograms::from_json(&d, &context()).err().unwrap();
    assert!(err.contains("cycle") && err.contains("cycle.hwv:5:") && err.contains("cycle.hwv:6:"));
    assert!(merge_lemma_source("lemmas \"bad\" { binding false; }", "bad.hwv", None).is_err());
    assert!(merge_lemma_source(SOURCE, "bad-base.hwv", Some(&json!({}))).is_err());
    assert!(merge_lemma_source(
        &SOURCE.replace("guard impl.x == 0u8;", ""),
        "missing.hwv",
        None
    )
    .unwrap_err()
    .to_string()
    .contains("missing guard"));
}
#[test]
fn real_rv_module_only_replaces_named_proposals() {
    let base = json!({"version":1,"mode":"independent_lemmas","variables":{},"lets":[{"id":"x_pre","expr":true},{"id":"x_post","expr":true}],"programs":[{"id":"x_operand1","match_rhs":["bv",8,0],"steps":[{"op":"candidate","id":"delivery","frame":"current_query","context":true,"guard":"let.x_pre","claim":"let.x_post","depends_on":[]},{"op":"project","id":"local","source":"delivery","path":[]},{"op":"use_candidate","id":"equality","candidate":"local","context":"$pre"}],"result":"equality"}]});
    let mut base = base;
    base["programs"].as_array_mut().unwrap().push(json!({"id":"m_address","match_rhs":["bv",8,0],"steps":[{"op":"candidate","id":"ir","frame":"current_query","context":["and","$pre","let.normal_m_address"],"guard":true,"claim":["eq","impl_next.m_ir","impl.x_ir"],"depends_on":[]}],"result":"ir"}));
    let source = include_str!("../../../audit/lemma_candidates/rv-delivery.hwv");
    let d = merge_lemma_source(source, "rv-delivery.hwv", Some(&base)).unwrap();
    assert_eq!(
        d["programs"][0]["steps"][1],
        base["programs"][0]["steps"][1]
    );
    assert_eq!(
        d["programs"][0]["match_rhs"],
        base["programs"][0]["match_rhs"]
    );
    let mut stripped = d.clone();
    for step in stripped["programs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .flat_map(|p| p["steps"].as_array_mut().unwrap())
    {
        if matches!(step["op"].as_str(), Some("candidate" | "use_candidate")) {
            step.as_object_mut().unwrap().remove("source");
        }
    }
    assert_eq!(stripped, base);
}
#[test]
fn native_guard_worker() {
    if std::env::var("NATIVE_LEMMA_CHILD").is_err() {
        assert!(Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_guard_worker", "--nocapture"])
            .env("NATIVE_LEMMA_CHILD", "1")
            .env("HWVERIFY_SOLVER", "finite")
            .status()
            .unwrap()
            .success());
        return;
    }
    let ctx = context();
    let metadata = merge_lemma_source(SOURCE, "guard-use.hwv", None).unwrap();
    let program = ProofPrograms::from_json(&metadata, &ctx).unwrap();
    for valid in [true, false] {
        let out = std::env::temp_dir().join(format!("native-lemma-{}-{valid}", std::process::id()));
        fs::create_dir_all(&out).unwrap();
        let mut check = Check {
            out: out.clone(),
            z3: "FORBIDDEN".into(),
            reports: vec![],
        };
        let g = eq(ctx["impl.x"].clone(), bv(8, 0));
        let bad = and(if valid { g.clone() } else { boolv(true) }, not(g));
        let result = program.try_query(&mut check, "native", &bad, &ctx);
        let d = &check.reports[0]["lemma_candidates"];
        if valid {
            assert!(result.unwrap());
            assert_eq!(d["target_closed"], true);
        } else {
            assert!(result.is_err());
            assert_eq!(
                d["uses"][0]["state"],
                "use_context_does_not_establish_guard"
            );
        }
        assert_eq!(d["uses"][0]["source"], "guard-use.hwv:6:9");
        fs::remove_dir_all(out).unwrap();
    }
    // A source-authored false claim fails before its use can receive a handle.
    let false_source = SOURCE.replace("claim impl.x == 0u8;", "claim impl.x == 1u8;");
    let metadata = merge_lemma_source(&false_source, "false-claim.hwv", None).unwrap();
    let program = ProofPrograms::from_json(&metadata, &ctx).unwrap();
    let out = std::env::temp_dir().join(format!("native-false-{}", std::process::id()));
    fs::create_dir_all(&out).unwrap();
    let mut check = Check {
        out: out.clone(),
        z3: "FORBIDDEN".into(),
        reports: vec![],
    };
    let g = eq(ctx["impl.x"].clone(), bv(8, 0));
    assert!(program
        .try_query(&mut check, "false_claim", &and(g.clone(), not(g)), &ctx)
        .is_err());
    let d = &check.reports[0]["lemma_candidates"];
    assert_eq!(d["candidates"][0]["validity"], "lemma_counterexample");
    assert_eq!(d["candidates"][0]["source"], "false-claim.hwv:5:11");
    assert!(d["candidates"][0]["claim_true"].is_null());
    assert!(d["uses"].as_array().unwrap().is_empty());
    fs::remove_dir_all(out).unwrap();
}
