#![cfg(feature = "host-runtime")]

use std::path::{Path, PathBuf};

use celox::{Simulator, TestResult};
use veryl_metadata::Metadata;

#[test]
fn axi4_stream_vip_runs_end_to_end() {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vip_registry");
    let mut metadata = Metadata::load(project.join("Veryl.toml")).unwrap();
    let paths = metadata.paths::<PathBuf>(&[], false, true).unwrap();
    let sources = paths
        .iter()
        .filter(|path| !path.example)
        .map(|path| {
            (
                std::fs::read_to_string(&path.src).unwrap(),
                path.src.clone(),
            )
        })
        .collect::<Vec<_>>();
    let source_refs = sources
        .iter()
        .map(|(source, path)| (source.as_str(), path.as_path()))
        .collect::<Vec<_>>();

    assert_eq!(
        Simulator::from_sources(source_refs, "vip_registry_test")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}
