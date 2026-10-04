//! Structural contracts over complete source-variable dependency artifacts.
//! Separate from behavioral/temporal noninterference and physical timing.
use hwverify_ir::{Sort, Specification};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub fn check(spec: &Specification, artifact: Option<&Value>) -> Result<Value, String> {
    let doc = spec.document();
    let selected = spec
        .implementation()
        .map(|i| spec.compositions()[&i.composition].members.clone())
        .unwrap_or_else(|| spec.components().keys().cloned().collect());
    let mut obligations = vec![];
    for component in selected {
        if let Some(rules) = doc["components"][&component]["structure"]["no_comb_path"].as_object()
        {
            for (name, rule) in rules {
                let path = format!("/components/{component}/structure/no_comb_path/{name}");
                let mut result = json!({"name":format!("{component}.{name}"),"source_path":path,"from":rule["from"],"to":rule["to"],"status":"unbound","witness":[]});
                if let Some(graph) = artifact {
                    evaluate(spec, graph, rule, &mut result)?;
                }
                obligations.push(result);
            }
        }
    }
    let status = if obligations.is_empty() {
        "not_requested"
    } else if obligations.iter().any(|r| r["status"] == "violated") {
        "violated"
    } else if obligations.iter().all(|r| r["status"] == "verified") {
        "verified"
    } else if obligations.iter().any(|r| r["status"] == "unsupported") {
        "unsupported"
    } else {
        "unbound"
    };
    Ok(
        json!({"status":status,"obligations":obligations,"claim":"Only structural reachability in the supplied complete elaborated source-variable graph; not functional/temporal noninterference, synthesized netlist or physical timing","artifact_design_sha256":artifact.and_then(|g|g.get("design_sha256"))}),
    )
}
fn evaluate(
    spec: &Specification,
    graph: &Value,
    rule: &Value,
    report: &mut Value,
) -> Result<(), String> {
    if graph["version"] != 1 || graph["scope"] != "elaborated_source_variable_graph" {
        return Err("unsupported structural artifact version/scope".into());
    }
    if graph["status"] != "complete" {
        report["status"] = json!("unsupported");
        report["reasons"] = graph["reasons"].clone();
        return Ok(());
    }
    if graph["coverage"].as_array().is_none_or(|v| v.is_empty())
        || graph["reasons"].as_array().is_none_or(|v| !v.is_empty())
        || graph["design_sha256"]
            .as_str()
            .is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        report["status"] = json!("unsupported");
        report["reason"] = json!("missing complete graph coverage/provenance");
        return Ok(());
    }
    let nodes = graph["nodes"].as_object().ok_or("missing graph nodes")?;
    let edges = graph["edges"].as_array().ok_or("missing graph edges")?;
    let cuts = graph["sequential_cuts"]
        .as_array()
        .ok_or("missing sequential cuts")?;
    if nodes.len() > 100_000 || edges.len() + cuts.len() > 1_000_000 {
        return Err("structural graph budget exceeded".into());
    }
    for edge in edges.iter().chain(cuts) {
        let from = edge["from"].as_str().ok_or("invalid graph source")?;
        let to = edge["to"].as_str().ok_or("invalid graph destination")?;
        if !nodes.contains_key(from)
            || !nodes.contains_key(to)
            || !matches!(
                edge["kind"].as_str(),
                Some("data" | "control" | "connection")
            )
        {
            return Err("invalid/uncovered graph edge".into());
        }
    }
    let mut endpoints = vec![];
    for field in ["from", "to"] {
        let port = rule[field].as_str().unwrap();
        let Some(actual) = spec.document()["implementation"]["endpoints"][port].as_str() else {
            return Ok(());
        };
        let Some(node) = nodes.get(actual) else {
            report["reason"] = json!(format!("unresolved source endpoint {actual}"));
            return Ok(());
        };
        let (env, local, direction) = if let Some(n) = port.strip_prefix("i.") {
            (spec.inputs(), n, "Input")
        } else {
            (spec.observations(), &port[2..], "Output")
        };
        let width = match env[local].0.sort {
            Sort::Bool => 1,
            Sort::Bv(w @ 1..=64) => w,
            _ => {
                report["status"] = json!("unsupported");
                return Ok(());
            }
        };
        if node["width"].as_u64() != Some(width as u64)
            || node["kind"] != direction
            || !matches!(node["type_kind"].as_str(), Some("Bit" | "Logic"))
        {
            return Err(format!(
                "source endpoint type/direction mismatch: {port} -> {actual}"
            ));
        }
        endpoints.push(actual);
    }
    // Direct scalar port connections identify the same source net. Retain
    // both traversal directions; assignment data/control edges stay directed.
    let mut connectivity = edges.clone();
    for edge in edges.iter().filter(|edge| edge["kind"] == "connection") {
        let mut reverse = edge.clone();
        reverse["from"] = edge["to"].clone();
        reverse["to"] = edge["from"].clone();
        connectivity.push(reverse);
    }
    let mut adjacency: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for edge in &connectivity {
        adjacency
            .entry(edge["from"].as_str().unwrap())
            .or_default()
            .push(edge);
    }
    let mut queue = VecDeque::from([endpoints[0]]);
    let mut seen = BTreeSet::from([endpoints[0]]);
    let mut parents: BTreeMap<&str, &Value> = BTreeMap::new();
    while let Some(node) = queue.pop_front() {
        if node == endpoints[1] {
            let mut witness = vec![];
            let mut at = node;
            while at != endpoints[0] {
                let edge = parents[at];
                witness.push(edge.clone());
                at = edge["from"].as_str().unwrap();
            }
            witness.reverse();
            report["status"] = json!("violated");
            report["witness"] = json!(witness);
            return Ok(());
        }
        for edge in adjacency.get(node).into_iter().flatten() {
            let next = edge["to"].as_str().unwrap();
            if seen.insert(next) {
                parents.insert(next, edge);
                queue.push_back(next);
            }
        }
    }
    report["status"] = json!("verified");
    report["reachable_nodes"] = json!(seen);
    report["sequential_cuts"] = json!(cuts.len());
    Ok(())
}
