//! Runtime corpus loading for the development executor.

use crate::{StdResolver, TestCase};
use std::path::Path;

#[allow(
    clippy::disallowed_methods,
    reason = "test corpus configuration boundary"
)]
pub(crate) fn load(language: &str, resolver: StdResolver) -> Vec<&'static TestCase> {
    let root = std::env::var_os("CELOX_VTEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    load_directory(&root.join(language).join("cases"), resolver)
}

fn load_directory(directory: &Path, resolver: StdResolver) -> Vec<&'static TestCase> {
    let mut paths = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read corpus entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "vtest")
        })
        .collect::<Vec<_>>();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "no .vtest files in {}",
        directory.display()
    );
    let mut names = std::collections::BTreeSet::new();
    paths
        .into_iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            crate::load_group(&path.to_string_lossy(), &text, resolver)
        })
        .inspect(|case| {
            assert!(
                names.insert(case.name),
                "duplicate vtest case: {}",
                case.name
            );
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(group: &str, source: &str) -> String {
        format!(
            "(group {group} (category operators)) (case t (source \"top.sv\" \"{source}\") (top Top))"
        )
    }

    #[test]
    fn discovers_files_in_order_and_reads_edited_sources() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("b.vtest"), case("b", "before")).unwrap();
        std::fs::write(directory.path().join("a.vtest"), case("a", "a")).unwrap();
        std::fs::write(directory.path().join("ignored.txt"), "not a vtest").unwrap();
        let cases = load_directory(directory.path(), crate::no_std_library);
        assert_eq!(
            cases.iter().map(|case| case.name).collect::<Vec<_>>(),
            ["a::t", "b::t"]
        );
        assert_eq!(cases[1].design().sources[0].text, "before");
        std::fs::write(directory.path().join("b.vtest"), case("b", "after")).unwrap();
        let cases = load_directory(directory.path(), crate::no_std_library);
        assert_eq!(cases[1].design().sources[0].text, "after");
    }

    #[test]
    #[should_panic(expected = "duplicate vtest case: a::t")]
    fn duplicate_names_are_errors() {
        let directory = tempfile::tempdir().unwrap();
        for file in ["a.vtest", "b.vtest"] {
            std::fs::write(directory.path().join(file), case("a", "")).unwrap();
        }
        load_directory(directory.path(), crate::no_std_library);
    }

    #[test]
    #[should_panic(expected = "no .vtest files")]
    fn empty_corpus_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        load_directory(directory.path(), crate::no_std_library);
    }

    #[test]
    #[should_panic(expected = "broken.vtest:")]
    fn malformed_cases_report_the_file() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("broken.vtest"), "(case").unwrap();
        load_directory(directory.path(), crate::no_std_library);
    }
}
