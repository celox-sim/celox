//! Aggregate scoped trace admission and symbolic implementation-product proofs
//! without changing the legacy checker. Generated identities stay private.
use hwverify_ir::{Res, ScopedSpecification};
use hwverify_solver::Check;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

/// Check every declared target against its own interface and examples. Each
/// validated lowering has one root composition and private, example-free leaves.
/// Query files are namespaced by stable target indices, never user identifiers.
pub fn check_scoped_specification(
    spec: &ScopedSpecification,
    z3: String,
    out: PathBuf,
) -> Res<Value> {
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut examples = vec![];
    let mut coverage = vec![];
    let mut binding = None;
    for (index, (name, target)) in spec.targets().iter().enumerate() {
        let directory = format!("target_{index:04}");
        let mut report =
            crate::check_specification(&target.specification, z3.clone(), out.join(&directory))?;
        let mut query = Check {
            z3: z3.clone(),
            out: out.join(&directory),
            reports: vec![],
        };
        report["implementation_binding"] =
            json!(crate::scoped_binding::obligations(target, &mut query)?);
        present_actions(&mut report, &target.action_inputs);
        present_report(&mut report, &target.instance_paths, &directory);
        for (field, destination) in [("examples", &mut examples), ("coverage", &mut coverage)] {
            for entry in report[field]
                .as_array()
                .ok_or_else(|| format!("internal scoped report for {name} has no {field} array"))?
            {
                // Leaves are implementation details, not independent coverage
                // targets. Only the root carries this declaration's examples.
                if entry["target_kind"] == "composition" && entry["target"] == name.as_str() {
                    let mut entry = entry.clone();
                    entry["target_kind"] = json!(target.kind);
                    destination.push(entry);
                }
            }
        }
        if !report["implementation_binding"].is_null() {
            if binding.is_some() {
                return Err(
                    "internal scoped lowering contains multiple implementation bindings".into(),
                );
            }
            let mut result = report["implementation_binding"].take();
            result["target"] = json!(name);
            result["target_kind"] = json!(target.kind);
            binding = Some(result);
        }
    }
    let status = aggregate_status(&examples, binding.as_ref());
    Ok(json!({
        "schema_version": 4,
        "status": status,
        "name": spec.document().get("name"),
        "examples": examples,
        "coverage": coverage,
        "implementation_binding": binding,
        "claim": "Example results concern only each target's supplied finite observational traces; the separate implementation_binding result concerns universal relational safety and inactive-private-state stuttering for its selected target",
        "semantics": {
            "interface": "Each declaration has local typed inputs and outputs; named instance connections compose those interfaces without sharing private state",
            "composition": "Exported actions activate sets of local leaf operations; active relations are conjoined, inactive leaves preserve private state, and distinct local operations of one leaf are incompatible. Omitted composition operation groups retain same-name synchronous shorthand",
            "initial": "Each instance's init and invariant hold at frame 0; no implicit reset operation",
            "timing": "Step k consumes inputs k and a singleton operation or simultaneous action set, relating outputs/state at frames k and k+1; observe constrains frame k+1",
            "projection": "Private state and omitted inputs/outputs are existential; negative examples require UNSAT for every hidden completion",
            "reporting": "File paths are relative to this report's output directory; private labels use private.<instance>.<field> (private_next for binding next state), optionally preceded by frameN; symbolic activation controls use stepN.action.<exported-action>; context symbol values retain the emitted SMT identifiers"
        },
        "limitations": [
            "Passing examples alone is not universal verification, specification adequacy, deadlock freedom, liveness, or implementation refinement",
            "Initial predicates define example starting states; no reset reachability or arbitrary future extension is inferred",
            "A verified implementation binding is a separate deterministic relational-safety/private-stuttering witness, not a claim that the implementation realizes positive examples or that idle outputs always stutter",
            "Rust lowering, the structural kernel and Z3 are trusted; emitted SMT obligations are replayable"
        ]
    }))
}

/// Give generated trace controls a semantic namespace, preserving the solver
/// symbol values and every original input. Binding contexts already use action.*
/// selectors and contain only the actual implementation input environment.
fn present_actions(value: &mut Value, actions: &BTreeMap<String, String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                present_actions(value, actions);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                if key == "context_symbols" {
                    if let Some(symbols) = value.as_object_mut() {
                        *symbols = std::mem::take(symbols)
                            .into_iter()
                            .map(|(label, symbol)| {
                                let renamed = label.split_once(".i.").and_then(|(step, input)| {
                                    step.strip_prefix("step").filter(|n| {
                                        !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())
                                    })?;
                                    actions
                                        .iter()
                                        .find(|(_, generated)| generated.as_str() == input)
                                        .map(|(action, _)| format!("{step}.action.{action}"))
                                });
                                (renamed.unwrap_or(label), symbol)
                            })
                            .collect();
                    }
                } else {
                    present_actions(value, actions);
                }
            }
        }
        _ => {}
    }
}

fn aggregate_status(examples: &[Value], binding: Option<&Value>) -> &'static str {
    let binding_status = binding.and_then(|b| b["status"].as_str());
    if examples.iter().any(|x| x["status"] == "failed") {
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
    }
}

/// Rewrite report metadata only. In particular, context_symbols' values are SMT
/// identifiers and must remain byte-for-byte identical to the emitted queries.
fn present_report(value: &mut Value, paths: &BTreeMap<String, String>, directory: &str) {
    match value {
        Value::Array(values) => {
            for value in values {
                present_report(value, paths, directory);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                match key.as_str() {
                    "members" => {
                        if let Some(members) = value.as_array_mut() {
                            for member in members {
                                if let Some(path) = member.as_str().and_then(|id| paths.get(id)) {
                                    *member = json!(path);
                                }
                            }
                        }
                    }
                    "context_symbols" => {
                        if let Some(symbols) = value.as_object_mut() {
                            *symbols = std::mem::take(symbols)
                                .into_iter()
                                .map(|(label, symbol)| (context_path(&label, paths), symbol))
                                .collect();
                        }
                    }
                    "evidence" | "solver_output" | "residual" if value.is_string() => {
                        *value = json!(format!("{directory}/{}", value.as_str().unwrap()));
                    }
                    _ => present_report(value, paths, directory),
                }
            }
        }
        _ => {}
    }
}

fn context_path(label: &str, paths: &BTreeMap<String, String>) -> String {
    let (prefix, local) = match label.split_once('.') {
        Some((frame, local))
            if frame
                .strip_prefix("frame")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())) =>
        {
            (&label[..=frame.len()], local)
        }
        _ => ("", label),
    };
    let Some((component, field)) = local.split_once('.') else {
        return label.to_string();
    };
    if let Some(path) = paths.get(component) {
        return format!("{prefix}private.{path}.{field}");
    }
    if let Some(path) = component.strip_suffix("_next").and_then(|id| paths.get(id)) {
        return format!("{prefix}private_next.{path}.{field}");
    }
    label.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_rewrites_nested_evidence_and_private_labels_only() {
        let paths = BTreeMap::from([("leaf0".into(), "pair.left".into())]);
        let mut report = json!({
            "examples": [{"members":["leaf0"], "evidence": {
                "evidence":"example_0.smt2", "solver_output":"example_0.out",
                "kernel":{"residual":"example_0.kernel-residual.smt2"},
                "context_symbols":{
                    "frame12.leaf0.x":"trace_private0_t12_v0",
                    "frame12.o.leaf0":"observation0",
                    "step0.i.leaf0":"input0"
                }
            }}],
            "implementation_binding":{"members":["leaf0"], "obligations":[{
                "evidence":"binding_operation_0.smt2",
                "context_symbols":{"leaf0.x":"leaf0_solver_symbol", "leaf0_next.x":"next0", "impl.x":"state0"}
            }]}
        });
        present_report(&mut report, &paths, "target_0007");
        let evidence = &report["examples"][0]["evidence"];
        assert_eq!(report["examples"][0]["members"], json!(["pair.left"]));
        assert_eq!(evidence["evidence"], "target_0007/example_0.smt2");
        assert_eq!(evidence["solver_output"], "target_0007/example_0.out");
        assert_eq!(
            evidence["kernel"]["residual"],
            "target_0007/example_0.kernel-residual.smt2"
        );
        assert_eq!(
            evidence["context_symbols"]["frame12.private.pair.left.x"],
            "trace_private0_t12_v0"
        );
        assert_eq!(
            evidence["context_symbols"]["frame12.o.leaf0"],
            "observation0"
        );
        assert_eq!(evidence["context_symbols"]["step0.i.leaf0"], "input0");
        let obligation = &report["implementation_binding"]["obligations"][0];
        assert_eq!(
            obligation["evidence"],
            "target_0007/binding_operation_0.smt2"
        );
        assert_eq!(
            obligation["context_symbols"]["private.pair.left.x"],
            "leaf0_solver_symbol"
        );
        assert_eq!(
            obligation["context_symbols"]["private_next.pair.left.x"],
            "next0"
        );
        assert_eq!(obligation["context_symbols"]["impl.x"], "state0");
    }

    #[test]
    fn aggregate_status_preserves_example_and_binding_priority() {
        for (examples, binding, expected) in [
            (vec![], None, "no_examples"),
            (
                vec![json!({"status":"passed"})],
                None,
                "spec_examples_passed",
            ),
            (
                vec![],
                Some(json!({"status":"verified"})),
                "binding_verified_no_examples",
            ),
            (
                vec![json!({"status":"passed"})],
                Some(json!({"status":"verified"})),
                "spec_examples_and_binding_verified",
            ),
            (vec![], Some(json!({"status":"unknown"})), "unknown"),
            (
                vec![json!({"status":"unknown"})],
                Some(json!({"status":"verified"})),
                "unknown",
            ),
            (
                vec![json!({"status":"unknown"})],
                Some(json!({"status":"failed"})),
                "implementation_binding_failed",
            ),
            (
                vec![json!({"status":"failed"})],
                Some(json!({"status":"failed"})),
                "spec_examples_failed",
            ),
        ] {
            assert_eq!(aggregate_status(&examples, binding.as_ref()), expected);
        }
    }

    #[test]
    fn private_context_namespace_preserves_reserved_and_next_like_aliases() {
        let paths = BTreeMap::from([
            ("leaf0".into(), "o".into()),
            ("leaf1".into(), "impl".into()),
            ("leaf2".into(), "a".into()),
            ("leaf3".into(), "a_next".into()),
        ]);
        let mut report = json!({"context_symbols":{
            "frame0.leaf0.x":"private_observation_alias",
            "frame0.o.x":"observation",
            "leaf1.x":"private_implementation_alias",
            "impl.x":"implementation",
            "leaf2_next.x":"private_a_next",
            "leaf3.x":"private_a_next_alias"
        }});
        present_report(&mut report, &paths, "target_0000");
        let labels = report["context_symbols"].as_object().unwrap();
        assert_eq!(labels.len(), 6);
        assert_eq!(labels["frame0.private.o.x"], "private_observation_alias");
        assert_eq!(labels["frame0.o.x"], "observation");
        assert_eq!(labels["private.impl.x"], "private_implementation_alias");
        assert_eq!(labels["impl.x"], "implementation");
        assert_eq!(labels["private_next.a.x"], "private_a_next");
        assert_eq!(labels["private.a_next.x"], "private_a_next_alias");
    }
}
