use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// An external lookup made while lowering a design.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct FileDependency {
    pub path: PathBuf,
    /// The design explicitly supplied an absolute path, fixing the lookup location.
    pub fixed_location: bool,
}

/// Compilation-owned external file tracking, explicitly passed to workers.
#[derive(Clone, Debug, Default)]
pub struct FileDependencyCollector(Arc<Mutex<Vec<FileDependency>>>);

impl FileDependencyCollector {
    pub fn take(&self) -> Vec<FileDependency> {
        let mut paths = std::mem::take(&mut *self.0.lock().unwrap());
        paths.sort();
        paths.dedup();
        paths
    }

    pub(crate) fn record(&self, path: &Path, fixed_location: bool) {
        self.0.lock().unwrap().push(FileDependency {
            path: path.to_path_buf(),
            fixed_location,
        });
    }
}

/// Capture paths using a collector supplied explicitly to the compilation.
/// Independent collectors support nested and concurrent compilations.
pub fn capture_file_dependencies<T>(
    compile: impl FnOnce(&FileDependencyCollector) -> T,
) -> (T, Vec<FileDependency>) {
    let collector = FileDependencyCollector::default();
    let result = compile(&collector);
    (result, collector.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicitly_shared_workers_record_into_one_compilation() {
        let ((), paths) = capture_file_dependencies(|collector| {
            std::thread::scope(|scope| {
                for name in ["a.hex", "b.hex"] {
                    let collector = collector.clone();
                    scope.spawn(move || collector.record(Path::new(name), false));
                }
            });
        });
        assert_eq!(
            paths
                .iter()
                .map(|entry| entry.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("a.hex"), Path::new("b.hex")]
        );
    }

    #[test]
    fn collectors_are_independent_and_deduplicate_fixed_lookups() {
        let ((), outer) = capture_file_dependencies(|collector| {
            collector.record(Path::new("/memory.hex"), false);
            let ((), inner) = capture_file_dependencies(|inner| {
                inner.record(Path::new("inner"), false);
            });
            assert_eq!(inner.len(), 1);
            collector.record(Path::new("/memory.hex"), true);
            collector.record(Path::new("/memory.hex"), true);
        });
        assert_eq!(
            outer,
            [
                FileDependency {
                    path: "/memory.hex".into(),
                    fixed_location: false
                },
                FileDependency {
                    path: "/memory.hex".into(),
                    fixed_location: true
                }
            ]
        );
    }
}
