//! The SystemVerilog suite: cases whose designs are SystemVerilog source.

mod cases;

#[cfg(feature = "external")]
pub mod verification;

use crate::TestCase;

/// All cases in deterministic declaration order. A case's design is
/// SystemVerilog source; build it with your SystemVerilog frontend.
pub fn cases() -> impl Iterator<Item = &'static TestCase> {
    static CASES: std::sync::OnceLock<Vec<&'static TestCase>> = std::sync::OnceLock::new();
    CASES
        .get_or_init(|| {
            cases::GROUPS
                .iter()
                .flat_map(|group| crate::load_group(group.file, group.text, crate::no_std_library))
                .collect()
        })
        .iter()
        .copied()
}

/// Look up a case by its full stable name.
pub fn case(name: &str) -> Option<&'static TestCase> {
    cases().find(|case| case.name == name)
}
