use std::path::{Path, PathBuf};

pub fn source(parts: &[&str]) -> String {
    veryl_std::expand().expect("failed to expand veryl-std sources");
    let rel = parts.iter().collect::<PathBuf>();
    let paths = veryl_std::paths(Path::new("")).expect("failed to resolve veryl-std sources");
    let src = paths
        .iter()
        .find(|path| path.src.ends_with(&rel))
        .unwrap_or_else(|| panic!("veryl-std source not found: {}", rel.display()));
    std::fs::read_to_string(&src.src)
        .unwrap_or_else(|err| panic!("failed to read {}: {}", src.src.display(), err))
}
