use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{Arc, Mutex},
};

/// An external lookup made while lowering a design.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct FileDependency {
    pub path: PathBuf,
    /// The design explicitly supplied an absolute path, fixing the lookup location.
    pub fixed_location: bool,
}

pub(crate) type Collector = Arc<Mutex<Vec<FileDependency>>>;

thread_local! {
    static FILES: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

/// Capture external file paths consulted by Veryl lowering, including its workers.
/// Includes absent lookup candidates, so a new file taking precedence invalidates
/// a cached compilation. Nested captures are independent; panics restore the
/// enclosing capture. Source files must be tracked separately by the caller.
pub fn capture_file_dependencies<T>(compile: impl FnOnce() -> T) -> (T, Vec<FileDependency>) {
    let files = Arc::new(Mutex::new(Vec::new()));
    let _restore = install(Some(files.clone()));
    let result = compile();
    let mut paths = std::mem::take(&mut *files.lock().unwrap());
    paths.sort();
    paths.dedup();
    (result, paths)
}

pub(crate) fn current() -> Option<Collector> {
    FILES.with(|files| files.borrow().clone())
}

pub(crate) fn install(collector: Option<Collector>) -> impl Drop {
    struct Restore(Option<Collector>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FILES.with(|files| *files.borrow_mut() = self.0.take());
        }
    }
    Restore(FILES.with(|files| files.replace(collector)))
}

pub(crate) fn record(path: &std::path::Path) {
    record_lookup(path, false);
}

pub(crate) fn record_absolute(path: &std::path::Path) {
    record_lookup(path, true);
}

fn record_lookup(path: &std::path::Path, fixed_location: bool) {
    FILES.with(|files| {
        if let Some(files) = files.borrow().as_ref() {
            files.lock().unwrap().push(FileDependency {
                path: path.to_path_buf(),
                fixed_location,
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(path: &str, fixed_location: bool) -> FileDependency {
        FileDependency {
            path: path.into(),
            fixed_location,
        }
    }

    #[test]
    fn distinguishes_fixed_and_relative_lookups() {
        let ((), paths) = capture_file_dependencies(|| {
            record(std::path::Path::new("/memory.hex"));
            record_absolute(std::path::Path::new("/memory.hex"));
            record_absolute(std::path::Path::new("/memory.hex"));
        });
        assert_eq!(
            paths,
            [
                dependency("/memory.hex", false),
                dependency("/memory.hex", true)
            ]
        );
    }

    #[test]
    fn nested_captures_restore_outer_collector() {
        let ((), outer) = capture_file_dependencies(|| {
            record(PathBuf::from("outer").as_path());
            let (42, inner) = capture_file_dependencies(|| {
                record(PathBuf::from("inner").as_path());
                42
            }) else {
                panic!("incorrect captured result");
            };
            assert_eq!(inner, [dependency("inner", false)]);
            record(PathBuf::from("outer").as_path());
        });
        assert_eq!(outer, [dependency("outer", false)]);
    }

    #[test]
    fn panic_restores_outer_collector() {
        let ((), outer) = capture_file_dependencies(|| {
            let result = std::panic::catch_unwind(|| {
                capture_file_dependencies(|| {
                    record(PathBuf::from("inner").as_path());
                    panic!("failed compilation");
                })
            });
            assert!(result.is_err());
            record(PathBuf::from("outer").as_path());
        });
        assert_eq!(outer, [dependency("outer", false)]);
    }
}
