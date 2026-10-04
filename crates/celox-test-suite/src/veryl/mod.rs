//! The Veryl suite: cases whose designs are Veryl source.

mod cases;

#[cfg(feature = "emit")]
pub mod emit;
#[cfg(feature = "external")]
pub mod verification;

use crate::TestCase;

/// The Icarus adapter for Veryl designs.
#[cfg(feature = "external")]
pub mod icarus {
    use crate::{Design, Result, TestCase};
    use std::path::Path;

    pub use crate::icarus::Icarus as Model;

    pub struct Icarus;

    impl Icarus {
        /// Emit `design` to SystemVerilog and build it.
        pub fn build(design: &Design, directory: &Path) -> Result<Model> {
            Model::build(&crate::veryl::emit::VerylFrontend, design, directory)
        }

        /// Build a case as a generated testbench.
        pub fn build_script(case: &TestCase, directory: &Path) -> Result<Model> {
            Model::build_script(&crate::veryl::emit::VerylFrontend, case, directory)
        }
    }
}

/// The Verilator adapter for Veryl designs.
#[cfg(feature = "external")]
pub mod verilator {
    use crate::{Design, Result, TestCase};
    use std::path::Path;

    pub use crate::verilator::Verilator as Model;

    pub struct Verilator;

    impl Verilator {
        /// Emit `design` to SystemVerilog and build it.
        pub fn build(design: &Design, directory: &Path) -> Result<Model> {
            Model::build(&crate::veryl::emit::VerylFrontend, design, directory)
        }

        /// Build a case as a generated testbench.
        pub fn build_script(case: &TestCase, directory: &Path) -> Result<Model> {
            Model::build_script(&crate::veryl::emit::VerylFrontend, case, directory)
        }
    }
}

/// The text of a Veryl standard library file such as `fifo/fifo.veryl`.
fn std_source(path: &str) -> String {
    use std::path::{Path, PathBuf};
    veryl_std::expand().expect("failed to expand veryl-std sources");
    let rel: PathBuf = path.split('/').collect();
    let paths = veryl_std::paths(Path::new("")).expect("failed to resolve veryl-std sources");
    let src = paths
        .iter()
        .find(|candidate| candidate.src.ends_with(&rel))
        .unwrap_or_else(|| panic!("veryl-std source not found: {path}"));
    std::fs::read_to_string(&src.src)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", src.src.display()))
}

/// All cases in deterministic declaration order. Filter by category or name in the
/// host test runner; nothing is silently skipped by the suite itself.
pub fn cases() -> impl Iterator<Item = &'static TestCase> {
    static CASES: std::sync::OnceLock<Vec<&'static TestCase>> = std::sync::OnceLock::new();
    CASES
        .get_or_init(|| {
            cases::GROUPS
                .iter()
                .flat_map(|group| crate::load_group(group.file, group.text, std_source))
                .collect()
        })
        .iter()
        .copied()
}

/// Look up a case by its full stable name.
pub fn case(name: &str) -> Option<&'static TestCase> {
    cases().find(|case| case.name == name)
}
