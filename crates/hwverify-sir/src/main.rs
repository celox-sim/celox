use std::{env, fs};
fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 2 || args.len() > 3 || args.get(2).is_some_and(|s| s != "--inline") {
        return Err("usage: hwverify-sir-lift COMPILED.json BINDINGS.json [--inline]".into());
    }
    let code = serde_json::from_slice(&fs::read(&args[0]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let config = serde_json::from_slice(&fs::read(&args[1]).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let result = hwverify_sir::lift(&code, &config)?.to_json(args.len() == 3)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("symbolic SIR lift failed: {error}");
        std::process::exit(2)
    }
}
