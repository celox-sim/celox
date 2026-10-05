//! Native structural contract checker over a preserved source graph artifact.
use serde_json::{Value, json};
use std::io::{self, Read};
fn main() {
    let result = (|| -> Result<Value, String> {
        let mut text = String::new();
        io::stdin()
            .take(16 * 1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > 16 * 1024 * 1024 {
            return Err("request too large".into());
        }
        let request: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if request["version"] != 1 {
            return Err("unsupported request version".into());
        }
        let uri = request["uri"].as_str().unwrap_or("structure.lyd");
        let parsed = request["source"]
            .as_str()
            .map(|s| lydite_syntax::parse_document(s, uri))
            .transpose()
            .map_err(|e| e.to_string())?;
        let doc = parsed
            .as_ref()
            .map(|p| &p.canonical)
            .unwrap_or(&request["document"]);
        let spec = lydite_ir::Specification::from_json(doc).map_err(|e| e.to_string())?;
        let mut report = lydite_verify::structure::check(&spec, request.get("artifact"))?;
        if let Some(parsed) = parsed {
            lydite_syntax::locate_report(&mut report, uri, &parsed.spans);
        }
        Ok(report)
    })();
    match result {
        Ok(value) => println!("{value}"),
        Err(error) => {
            println!(
                "{}",
                json!({"status":"invalid_structural_request","error":error})
            );
            std::process::exit(2);
        }
    }
}
