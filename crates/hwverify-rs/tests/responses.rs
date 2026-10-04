use serde_json::{json, Value};
use std::{fs, path::PathBuf, process::Command};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn source(scoped: bool) -> String {
    fs::read_to_string(root().join(if scoped {
        "examples/scoped_response.hwv"
    } else {
        "examples/response.hwv"
    }))
    .unwrap()
}
fn document(scoped: bool) -> Value {
    hwverify_syntax::parse_document(&source(scoped), "response.hwv")
        .unwrap()
        .canonical
}
fn check(doc: &Value, name: &str) -> Value {
    let out = root()
        .join("target/response-contract-tests")
        .join(format!("{}-{name}", std::process::id()));
    fs::create_dir_all(&out).unwrap();
    let input = out.join("input.json");
    fs::write(&input, serde_json::to_vec(doc).unwrap()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
        .env("HWVERIFY_SOLVER", "finite")
        .arg(input)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&result.stderr)));
    assert!(report.get("error").is_none(), "{report}");
    report
}
fn obligation<'a>(r: &'a Value, suffix: &str) -> &'a Value {
    r["implementation_binding"]["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"].as_str().is_some_and(|n| n.ends_with(suffix)))
        .unwrap()
}
#[test]
fn actual_progress_v3_v4_and_immediate_completion() {
    for scoped in [false, true] {
        let doc = document(scoped);
        let r = check(&doc, &format!("positive-{scoped}"));
        assert_eq!(r["implementation_binding"]["status"], "verified", "{r}");
        assert_eq!(
            r["implementation_binding"]["responses"][0]["status"],
            "verified"
        );
        assert_eq!(r["implementation_binding"]["responses"][0]["bound"], 2);
        // Independent concrete trace: acceptance, one waiting edge, completion.
        let mut busy = false;
        let mut ticks = 0u8;
        let mut count = 0u8;
        let mut completions = vec![];
        for edge in 0..3 {
            let accept = edge == 0 && !busy;
            let complete = busy && ticks == 0;
            if complete {
                count += 1;
                completions.push(edge);
            }
            ticks = if accept {
                1
            } else if busy {
                ticks.wrapping_sub(1) & 3
            } else {
                ticks
            };
            busy = (busy || accept) && !complete;
        }
        assert_eq!(completions, vec![2]);
        assert_eq!(count, 1);
        assert!(!busy);
        let mut immediate = doc.clone();
        immediate["implementation"]["wires"]["complete"] = json!("w.accept");
        immediate["implementation"]["next"]["busy"] = json!(false);
        immediate["implementation"]["next"]["ticks"] = json!(["bv", 2, 0]);
        immediate["implementation"]["responses"]["request_done"]["pending"] = json!(false);
        immediate["implementation"]["responses"]["request_done"]["rank"] = json!(["bv", 2, 0]);
        immediate["implementation"]["responses"]["request_done"]["bound"] = json!(1);
        assert_eq!(
            check(&immediate, &format!("immediate-{scoped}"))["implementation_binding"]["status"],
            "verified"
        );
    }
}
#[test]
fn rejects_stutter_deadlock_dropped_requests_overlap_and_bad_reset() {
    for scoped in [false, true] {
        let base = document(scoped);
        let mut idle = base.clone();
        idle["implementation"]["operations"]["advance"] = json!(false);
        for field in ["count", "busy", "ticks"] {
            idle["implementation"]["next"][field] = json!(format!("s.{field}"));
        }
        let r = check(&idle, &format!("idle-{scoped}"));
        assert_eq!(r["implementation_binding"]["status"], "failed");
        assert_eq!(
            obligation(&r, "pending_preserved")["status"],
            "counterexample"
        );
        let mut safety_only = idle.clone();
        safety_only["implementation"]
            .as_object_mut()
            .unwrap()
            .remove("responses");
        assert_eq!(
            check(&safety_only, &format!("idle-safety-only-{scoped}"))["implementation_binding"]
                ["status"],
            "verified"
        );
        let mut dropped = base.clone();
        dropped["implementation"]["next"]["busy"] = json!(false);
        assert_eq!(
            obligation(
                &check(&dropped, &format!("drop-{scoped}")),
                "pending_preserved"
            )["status"],
            "counterexample"
        );
        let mut dead = base.clone();
        dead["implementation"]["wires"]["complete"] = json!(false);
        dead["implementation"]["next"]["ticks"] = json!("s.ticks");
        assert_eq!(
            obligation(
                &check(&dead, &format!("dead-{scoped}")),
                "countdown_decreases"
            )["status"],
            "counterexample"
        );
        let mut overlap = base.clone();
        overlap["implementation"]["responses"]["request_done"]["accept"] = json!("i.request");
        assert_eq!(
            obligation(
                &check(&overlap, &format!("overlap-{scoped}")),
                "no_overlapping_acceptance"
            )["status"],
            "counterexample"
        );
        let mut reset = base.clone();
        reset["implementation"]["reset"]["busy"] = json!(true);
        assert_eq!(
            obligation(&check(&reset, &format!("reset-{scoped}")), "reset_cancels")["status"],
            "counterexample"
        );
        let mut bad_safety = base.clone();
        bad_safety["implementation"]["next"]["count"] = json!(["bv", 4, 3]);
        let failed_safety = check(&bad_safety, &format!("bad-safety-{scoped}"));
        assert_eq!(
            failed_safety["implementation_binding"]["responses"][0]["status"],
            "not_established_due_to_binding_failure"
        );
        let mut short = base.clone();
        short["implementation"]["responses"]["request_done"]["bound"] = json!(1);
        assert_eq!(
            obligation(
                &check(&short, &format!("bound-{scoped}")),
                "countdown_bound"
            )["status"],
            "counterexample"
        );
    }
}
#[test]
fn assumptions_are_nonvacuous_and_stalls_are_counted() {
    for scoped in [false, true] {
        for (name, assume) in [
            (
                "contradictory",
                json!(["and", "i.stall", ["not", "i.stall"]]),
            ),
            ("reset_only", json!("i.rst")),
        ] {
            let mut doc = document(scoped);
            doc["implementation"]["responses"]["request_done"]["assume"] = assume;
            let report = check(&doc, &format!("{name}-{scoped}"));
            assert_eq!(
                obligation(&report, "environment_nonempty")["status"],
                "failed_nonvacuity"
            );
            assert_eq!(report["implementation_binding"]["status"], "failed");
        }
        let mut doc = document(scoped);
        doc["implementation"]["responses"]["request_done"]["assume"] = json!(true);
        assert_eq!(
            obligation(
                &check(&doc, &format!("stall-{scoped}")),
                "countdown_decreases"
            )["status"],
            "counterexample"
        );
        doc = document(scoped);
        doc["implementation"]["responses"]["request_done"]["accept"] = json!(false);
        assert_eq!(
            obligation(
                &check(&doc, &format!("noaccept-{scoped}")),
                "accept_nonempty"
            )["status"],
            "failed_nonvacuity"
        );
    }
}
#[test]
fn rejects_unsupported_contracts_and_native_errors_have_spans() {
    for scoped in [false, true] {
        for (field, value) in [
            ("assume", json!("s.busy")),
            ("pending", json!("i.stall")),
            ("rank", json!(true)),
            ("bound", json!(0)),
            ("bound", json!(4)),
            ("operation", json!("missing")),
            ("queue_depth", json!(2)),
        ] {
            let mut doc = document(scoped);
            doc["implementation"]["responses"]["request_done"][field] = value;
            let error = if scoped {
                hwverify_ir::ScopedSpecification::from_json(&doc)
                    .err()
                    .unwrap()
            } else {
                hwverify_ir::Specification::from_json(&doc).err().unwrap()
            };
            assert!(
                error.path.starts_with("/implementation/responses"),
                "{error}"
            );
        }
        let mut multiple = document(scoped);
        multiple["implementation"]["responses"]["second"] =
            multiple["implementation"]["responses"]["request_done"].clone();
        assert!(if scoped {
            hwverify_ir::ScopedSpecification::from_json(&multiple).is_err()
        } else {
            hwverify_ir::Specification::from_json(&multiple).is_err()
        });
        let text = source(scoped).replace("assume !i.stall;", "assume s.busy;");
        let parsed = hwverify_syntax::parse_document(&text, "source.hwv").unwrap();
        let error = if scoped {
            parsed.validate_scoped_specification().err().unwrap()
        } else {
            parsed.validate_specification().err().unwrap()
        };
        assert!(error.span.is_some());
        assert!(error.to_string().contains("source.hwv:"));
    }
}
#[test]
fn cli_progress_obligations_have_native_locations() {
    let out = root().join("target/response-contract-native");
    let result = Command::new(env!("CARGO_BIN_EXE_hwverify-rs"))
        .env("HWVERIFY_SOLVER", "finite")
        .arg(root().join("examples/scoped_response.hwv"))
        .arg("--out")
        .arg(out)
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    let location = &obligation(&report, "countdown_decreases")["source_location"];
    assert!(location["uri"]
        .as_str()
        .unwrap()
        .ends_with("scoped_response.hwv"));
    assert!(location["span"]["line"].as_u64().unwrap() > 30);
}

#[test]
fn unreachable_acceptance_does_not_establish_service_after_reset() {
    for scoped in [false, true] {
        // Reset and every transition preserve !ever_enabled. Acceptance is therefore
        // unreachable for every input sequence, although safe arbitrary states can accept.
        let text = source(scoped)
            .replace(
                "state busy: bool;",
                "state busy: bool; state ever_enabled: bool;",
            )
            .replace("reset {", "reset { ever_enabled = false;")
            .replace("next {", "next { ever_enabled = s.ever_enabled;")
            .replace(
                "accept = i.request && !s.busy;",
                "accept = i.request && !s.busy && s.ever_enabled;",
            );
        let doc = hwverify_syntax::parse_document(&text, "unreachable.hwv")
            .unwrap()
            .canonical;
        let r = check(&doc, &format!("unreachable-accept-{scoped}"));
        assert_eq!(r["implementation_binding"]["status"], "verified", "{r}");
        assert_eq!(obligation(&r, "accept_nonempty")["status"], "passed");
        let response = &r["implementation_binding"]["responses"][0];
        assert_eq!(response["status"], "verified");
        assert_eq!(
            response["adequacy"]["reset_reachable_acceptance"],
            "unchecked"
        );
        assert_eq!(
            response["adequacy"]["external_request_to_acceptance"],
            "not_specified"
        );
    }
}
