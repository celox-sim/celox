//! Isolated, finite-only editor worker. One request, no persistent proof handles.
use hwverify_ir::Design;
use hwverify_syntax::{SyntaxError, parse_document};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read},
    path::Path,
};

fn diagnostic(e: SyntaxError) -> Value {
    json!({"uri":e.filename,"message":e.message,"span":e.span.map(|s|json!({"start":s.start,"end":s.end,"line":s.line,"column":s.column}))})
}
fn parse(source: &str, uri: &str) -> Result<Value, Value> {
    if source.trim_start().starts_with('{') {
        let doc = hwverify_syntax::parse_json(source.as_bytes())
            .map_err(|e| json!({"uri":uri,"message":e}))?;
        if doc["kind"] == "specification" {
            return Err(json!({"uri":uri,"message":"associate a design, not a specification"}));
        }
        Design::from_json(&doc).map_err(|e| json!({"uri":uri,"message":e.to_string()}))?;
        return Ok(doc);
    }
    let parsed = parse_document(source, uri).map_err(diagnostic)?;
    if parsed.canonical["version"] == 4 {
        parsed.validate_scoped_specification().map_err(diagnostic)?;
    } else if parsed.canonical["kind"] == "specification" {
        parsed.validate_specification().map_err(diagnostic)?;
    } else {
        parsed.validate().map_err(diagnostic)?;
    }
    Ok(parsed.canonical)
}
fn run(request: &Value) -> Result<Value, Value> {
    let uri = request["uri"]
        .as_str()
        .ok_or(json!({"message":"missing URI"}))?;
    let source = request["text"]
        .as_str()
        .ok_or(json!({"message":"missing text"}))?;
    let doc = if let Some(base) = request.get("base") {
        let base_uri = base["uri"]
            .as_str()
            .ok_or(json!({"message":"missing base URI"}))?;
        // The server supplies a private, identity-checked file snapshot for disk
        // models, avoiding a second enormous JSON encoding of generated designs.
        let base_text = if let Some(text) = base["text"].as_str() {
            text.to_owned()
        } else {
            fs::read_to_string(
                base["path"]
                    .as_str()
                    .ok_or(json!({"message":"missing base snapshot"}))?,
            )
            .map_err(|e| json!({"uri":base_uri,"message":e.to_string()}))?
        };
        let mut doc = parse(&base_text, base_uri)?;
        doc["proof_programs"] =
            hwverify_syntax::merge_lemma_source(source, uri, doc.get("proof_programs"))
                .map_err(diagnostic)?;
        doc
    } else {
        parse(source, uri)?
    };
    if doc["kind"] == "specification" {
        if request["operation"] == "prove" {
            if request["program"] != "responses"
                || !request["step"].is_null()
                || !request["branch"].is_null()
            {
                return Err(
                    json!({"uri":uri,"message":"select Check implementation responses; steps and branches are unsupported for response contracts"}),
                );
            }
            let out = Path::new(
                request["out"]
                    .as_str()
                    .ok_or(json!({"message":"missing output"}))?,
            );
            fs::create_dir_all(out).map_err(|e| json!({"message":e.to_string()}))?;
            let mut report = if doc["version"] == 4 {
                let spec = hwverify_ir::ScopedSpecification::from_json(&doc)
                    .map_err(|e| json!({"message":e.to_string()}))?;
                hwverify_verify::check_scoped_specification(
                    &spec,
                    "EDITOR_EXTERNAL_SOLVER_FORBIDDEN".into(),
                    out.into(),
                )
            } else {
                let spec = hwverify_ir::Specification::from_json(&doc)
                    .map_err(|e| json!({"message":e.to_string()}))?;
                hwverify_verify::check_specification(
                    &spec,
                    "EDITOR_EXTERNAL_SOLVER_FORBIDDEN".into(),
                    out.into(),
                )
            }
            .map_err(|e| json!({"uri":uri,"message":e}))?;
            let parsed = parse_document(source, uri).map_err(diagnostic)?;
            if report["implementation_binding"].is_object() {
                report["implementation_binding"]["source_path"] = json!("/implementation");
            }
            hwverify_syntax::locate_report(&mut report, uri, &parsed.spans);
            let mut diagnostics=report["implementation_binding"]["obligations"].as_array().into_iter().flatten()
                .filter(|r|r.get("source_path").is_some()).map(|r|json!({
                    "uri":uri,"span":r["source_location"]["span"],"code":r["name"],
                    "severity":if r["status"]=="passed" {3} else {1},
                    "message":format!("Response {} / {}: {}",r["response"].as_str().unwrap_or(""),r["name"].as_str().unwrap_or(""),r["status"].as_str().unwrap_or("unknown"))
                })).collect::<Vec<_>>();
            for obligation in report["structural"]["obligations"]
                .as_array()
                .into_iter()
                .flatten()
            {
                diagnostics.push(json!({"uri":uri,"span":obligation["source_location"]["span"],"code":"structural_unbound","severity":1,
                    "message":"no_comb_path is unbound: run the source structural checker with a complete source graph; behavioral proof does not discharge this obligation"}));
            }
            for response in report["implementation_binding"]["responses"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let adequacy = &response["adequacy"];
                let cover = &adequacy["reset_acceptance_cover"];
                let depth = cover["depth"]
                    .as_u64()
                    .map(|n| format!("depth {n} nonreset edges"))
                    .unwrap_or_else(|| "no depth requested".into());
                diagnostics.push(json!({"uri":uri,"span":cover["source_location"]["span"],
                    "code":"response_reset_acceptance_cover","severity":if cover["status"]=="reached" {3} else {2},
                    "message":format!("Reset-acceptance cover: {}; {}; bounded existential adequacy only. {}", cover["status"].as_str().unwrap_or("unknown"), depth, cover["reason"].as_str().unwrap_or(""))}));

                diagnostics.push(json!({"uri":uri,"span":adequacy["source_location"]["span"],
                    "code":"response_external_service_not_specified","severity":2,
                    "message":adequacy["message"]}));
            }
            if let Some(diagnostic) =
                binding_failure_diagnostic(&report["implementation_binding"], uri)
            {
                diagnostics.push(diagnostic);
            }
            let mut witnesses = serde_json::Map::new();
            for obligation in report["implementation_binding"]["obligations"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if obligation["status"] == "counterexample"
                    && obligation["finite"]["original_formula_validated"] == true
                    && obligation["solver_result"] == "sat"
                {
                    if let Some(evidence) = obligation["evidence"].as_str() {
                        if let Ok(bytes) =
                            fs::read(out.join(evidence).with_extension("finite.json"))
                        {
                            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                                witnesses.insert(obligation["name"].as_str().unwrap_or(evidence).into(),json!({"context":value["context_values"],"source_location":obligation["source_location"],"original_formula_validated":true,"reset_reachability":"not_checked"}));
                            }
                        }
                    }
                }
            }
            return Ok(
                json!({"diagnostics":diagnostics,"verification":report,"witnesses":witnesses,"proof_programs":null,"binding":null}),
            );
        }
        return Ok(json!({"diagnostics":[],"proof_programs":null,"binding":null}));
    }
    let design = Design::from_json(&doc).map_err(|e| json!({"uri":uri,"message":e.to_string()}))?;
    hwverify_verify::validate_proof_metadata(&design)
        .map_err(|e| json!({"uri":uri,"message":e}))?;
    if request["operation"] != "prove" {
        return Ok(
            json!({"diagnostics":[],"proof_programs":doc.get("proof_programs"),"binding":doc.get("binding")}),
        );
    }
    let step = match request.get("step") {
        None | Some(Value::Null) => None,
        Some(Value::String(step)) => Some(step.as_str()),
        _ => return Err(json!({"uri":uri,"message":"step must be a declaration name"})),
    };
    let branch = match request.get("branch") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or(json!({"uri":uri,"message":"branch must be a nonnegative integer"}))?,
        ),
    };
    let out = Path::new(
        request["out"]
            .as_str()
            .ok_or(json!({"message":"missing evidence output"}))?,
    );
    fs::create_dir_all(out).map_err(|e| json!({"message":e.to_string()}))?;
    let result = hwverify_verify::check_editor_request(
        &design,
        request["program"]
            .as_str()
            .ok_or(json!({"message":"missing target"}))?,
        step,
        branch,
        out.into(),
    )
    .map_err(|e| json!({"uri":uri,"message":e}))?;
    let mut witnesses = serde_json::Map::new();
    if let Some(reports) = result["reports"].as_array() {
        for report in reports {
            for child in report["children"].as_array().into_iter().flatten() {
                if child["solver_result"] == "sat"
                    && child["finite"]["original_formula_validated"] == true
                {
                    if let Some(name) = child["name"].as_str() {
                        let path = out.join(format!("{name}.finite.json"));
                        if let Ok(bytes) = fs::read(path) {
                            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                                witnesses.insert(name.into(),json!({"context":v["context_values"],"assignments":v["assignments"],"original_formula_validated":true}));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(
        json!({"diagnostics":[],"proof":result,"witnesses":witnesses,"proof_programs":doc["proof_programs"],"binding":doc["binding"]}),
    )
}
fn main() {
    // Reproducible editor settings; no inherited automatic search or raised budget.
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("HWVERIFY_")) {
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(key) };
    }
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HWVERIFY_SOLVER", "finite") };
    let mut bytes = vec![];
    let response = match io::stdin()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
    {
        Ok(_) if bytes.len() <= 64 * 1024 * 1024 => match serde_json::from_slice::<Value>(&bytes) {
            Ok(request) => run(&request).unwrap_or_else(|e| json!({"diagnostics":[e]})),
            Err(e) => json!({"diagnostics":[{"message":e.to_string()}]}),
        },
        _ => json!({"diagnostics":[{"message":"editor request exceeds 64 MiB or cannot be read"}]}),
    };
    println!("{response}");
}

fn binding_failure_diagnostic(binding: &Value, uri: &str) -> Option<Value> {
    let status = binding["status"].as_str()?;
    if status == "verified" {
        return None;
    }
    let blockers = binding["obligations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["status"] != "passed")
        .map(|r| {
            format!(
                "{}: {}",
                r["name"].as_str().unwrap_or("obligation"),
                r["status"].as_str().unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(json!({"uri":uri,"span":binding["source_location"]["span"],
        "code":"implementation_binding_not_verified","severity":1,
        "message":format!("Implementation binding {status}; conditional response guarantee is not established. {blockers}")}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_and_unknown_prerequisites_are_blocking_diagnostics() {
        for status in ["failed", "unknown"] {
            let binding = json!({"status":status,"source_location":{"span":{"line":12}},
                "obligations":[{"name":"response_0_countdown_decreases","status":"passed"},
                    {"name":"binding_step","status":status}]});
            let diagnostic = binding_failure_diagnostic(&binding, "test.hwv").unwrap();
            assert_eq!(diagnostic["severity"], 1);
            assert_eq!(diagnostic["span"]["line"], 12);
            assert!(
                diagnostic["message"]
                    .as_str()
                    .unwrap()
                    .contains("binding_step")
            );
        }
        assert!(binding_failure_diagnostic(&json!({"status":"verified"}), "test.hwv").is_none());
    }
}
