use celox_test_suite_veryl::{capture, cases};
fn main() {
    // Unsupported reads/panics are structured per-case results, not console noise.
    std::panic::set_hook(Box::new(|_| {}));
    let filters: Vec<_> = std::env::args().skip(1).collect();
    let rows: Vec<_> = cases()
        .filter(|case| filters.is_empty() || filters.iter().any(|x| x == case.name))
        .map(capture::run_case)
        .collect();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "schema": "celox-assertion-capture-v1", "cases": rows,
        "semantics": "original Rust expected-value/stimulus evaluation; hardware reads never return concrete values"
    })).unwrap());
}
