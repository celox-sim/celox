//! Bounded failure discovery and strict replay; no simulator or external solver.
use serde_json::{Value, json};
use std::io::{self, Read};
fn run(v: &Value) -> Result<Value, String> {
    if v["version"] != 1 {
        return Err("unsupported request version".into());
    }
    let doc = if let Some(source) = v["source"].as_str() {
        hwverify_syntax::parse_document(source, "replay.hwv")
            .map_err(|e| e.to_string())?
            .canonical
    } else {
        v["document"].clone()
    };
    let spec = hwverify_ir::Specification::from_json(&doc).map_err(|e| e.to_string())?;
    match v["mode"].as_str() {
        Some("induct") => hwverify_verify::induction::check_inductive_safety(
            &spec,
            v.get("candidates").ok_or("missing candidates")?,
            v["out"]
                .as_str()
                .ok_or("missing proof output directory")?
                .into(),
        ),
        Some("search") => hwverify_verify::reachable::search_reachable(
            &spec,
            v["goal"].as_str().ok_or("missing goal")?,
            v["depth"]
                .as_u64()
                .filter(|n| *n <= 32)
                .ok_or("invalid depth")? as u32,
        ),
        Some("check_stimulus") => {
            let signals = match v.get("signals") {
                None => vec![],
                Some(Value::Array(xs)) => xs
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_owned)
                            .ok_or("sampled signal must be a string")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err("signals must be an array".into()),
            };
            hwverify_verify::reachable::observe_stimulus(
                &spec,
                v["goal"].as_str().ok_or("missing goal")?,
                v["inputs"].as_array().ok_or("missing inputs")?,
                &signals,
            )
        }
        Some("replay") => hwverify_verify::reachable::validate_reachable(&spec, &v["witness"]),
        _ => Err("mode must be search, check_stimulus, replay or induct".into()),
    }
}
fn main() {
    let result = (|| -> Result<Value, String> {
        let mut b = String::new();
        io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut b)
            .map_err(|e| e.to_string())?;
        if b.len() > 16 * 1024 * 1024 {
            return Err("request too large".into());
        }
        run(&serde_json::from_str(&b).map_err(|e| e.to_string())?)
    })();
    match result {
        Ok(v) => println!("{v}"),
        Err(e) => {
            println!("{}", json!({"status":"invalid_replay_request","error":e}));
            std::process::exit(2);
        }
    }
}
