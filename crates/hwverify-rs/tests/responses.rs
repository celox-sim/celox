use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../hwverify")
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
            check(&safety_only, &format!("idle-safety-only-{scoped}"))["implementation_binding"]["status"],
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
    assert!(
        location["uri"]
            .as_str()
            .unwrap()
            .ends_with("scoped_response.hwv")
    );
    assert!(location["span"]["line"].as_u64().unwrap() > 30);
    assert!(
        cover(&report)["source_location"]["span"]["line"]
            .as_u64()
            .unwrap()
            > 30
    );
    assert_eq!(cover(&report)["status"], "reached");
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
            "not_reached_within_bound"
        );
        assert_eq!(
            response["adequacy"]["external_request_to_acceptance"],
            "not_specified"
        );
    }
}

fn cover(r: &Value) -> &Value {
    &r["implementation_binding"]["responses"][0]["adequacy"]["reset_acceptance_cover"]
}
#[test]
fn reset_cover_depth_and_assumptions_have_exact_edge_indexing() {
    for scoped in [false, true] {
        let text = source(scoped)
            .replace(
                "state busy: bool;",
                "state busy: bool; state enabled: bool;",
            )
            .replace("reset {", "reset { enabled = false;")
            .replace("next {", "next { enabled = true;")
            .replace(
                "accept = i.request && !s.busy;",
                "accept = i.request && !s.busy && s.enabled;",
            );
        let mut delayed = hwverify_syntax::parse_document(&text, "delayed.hwv")
            .unwrap()
            .canonical;
        delayed["implementation"]["responses"]["request_done"]["cover_depth"] = json!(1);
        let short = check(&delayed, &format!("cover-short-{scoped}"));
        assert_eq!(cover(&short)["status"], "not_reached_within_bound");
        assert!(cover(&short)["witness"].is_null());
        assert_eq!(short["implementation_binding"]["status"], "verified");
        delayed["implementation"]["responses"]["request_done"]["cover_depth"] = json!(2);
        let reached = check(&delayed, &format!("cover-depth-two-{scoped}"));
        assert_eq!(cover(&reached)["status"], "reached");
        assert_eq!(cover(&reached)["witness"]["acceptance_edge"], 2);
        assert_eq!(
            cover(&reached)["witness"]["original_transitions_validated"],
            true
        );
        let trace = cover(&reached)["witness"]["trace"].as_array().unwrap();
        assert_eq!(trace.len(), 3);
        assert_eq!(trace[0]["inputs"]["rst"]["value"], true);
        assert!(trace[0]["assumption"].is_null());
        assert_eq!(trace[1]["accept"], false);
        assert_eq!(trace[2]["accept"], true);
        assert_eq!(trace[1]["state_after"], trace[2]["state_before"]);
        // Reaching enabled would require violating the assumption BEFORE acceptance.
        delayed["implementation"]["next"]["enabled"] = json!(["or", "i.stall", "s.enabled"]);
        let blocked = check(&delayed, &format!("cover-prefix-assume-{scoped}"));
        assert_eq!(cover(&blocked)["status"], "not_reached_within_bound");
        // Reset inputs are independent: rst and stall may both be true on edge 0.
        delayed["implementation"]["reset"]["enabled"] = json!("i.stall");
        delayed["implementation"]["responses"]["request_done"]["assume"] =
            json!(["and", ["not", "i.rst"], ["not", "i.stall"]]);
        let seeded = check(&delayed, &format!("cover-reset-input-{scoped}"));
        assert_eq!(cover(&seeded)["status"], "reached");
        assert_eq!(
            cover(&seeded)["witness"]["trace"][0]["inputs"]["stall"]["value"],
            true
        );
        // Neither reset nor an assumption-violating acceptance can count as a hit.
        for (name, accept) in [("reset", "i.rst"), ("stall", "i.stall")] {
            delayed["implementation"]["responses"]["request_done"]["accept"] = json!(accept);
            assert_eq!(
                cover(&check(&delayed, &format!("cover-no-{name}-{scoped}")))["status"],
                "not_reached_within_bound"
            );
        }
    }
}
#[test]
fn cover_is_optional_and_exhausted_budget_is_unknown() {
    for scoped in [false, true] {
        let mut doc = document(scoped);
        let reached = check(&doc, &format!("cover-real-{scoped}"));
        assert_eq!(cover(&reached)["status"], "reached");
        assert_eq!(cover(&reached)["depth"], 2);
        assert_eq!(
            cover(&reached)["witness"]["original_transitions_validated"],
            true
        );
        doc["implementation"]["responses"]["request_done"]
            .as_object_mut()
            .unwrap()
            .remove("cover_depth");
        assert_eq!(
            cover(&check(&doc, &format!("cover-absent-{scoped}")))["status"],
            "unchecked"
        );
        let mut crowded = source(scoped).replace("cover_depth 2;", "cover_depth 32;");
        for n in 0..130 {
            crowded = crowded
                .replace(
                    "state busy: bool;",
                    &format!("state extra{n}: bool; state busy: bool;"),
                )
                .replace("reset {", &format!("reset {{ extra{n} = false;"))
                .replace("next {", &format!("next {{ extra{n} = s.extra{n};"));
        }
        let doc = hwverify_syntax::parse_document(&crowded, "crowded.hwv")
            .unwrap()
            .canonical;
        let unknown = check(&doc, &format!("cover-unknown-{scoped}"));
        assert_eq!(cover(&unknown)["status"], "unknown");
        assert!(cover(&unknown)["witness"].is_null());
        assert!(
            cover(&unknown)["reason"]
                .as_str()
                .unwrap()
                .contains("budget")
        );
        for bad in [json!(0), json!(33), json!(1.5)] {
            let mut invalid = document(scoped);
            invalid["implementation"]["responses"]["request_done"]["cover_depth"] = bad;
            let message = if scoped {
                hwverify_ir::ScopedSpecification::from_json(&invalid)
                    .unwrap_err()
                    .to_string()
            } else {
                hwverify_ir::Specification::from_json(&invalid)
                    .unwrap_err()
                    .to_string()
            };
            assert!(message.contains("cover_depth"), "{message}");
        }
    }
}
