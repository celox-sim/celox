use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub(crate) type Collector = Arc<Mutex<Vec<PathBuf>>>;

thread_local! {
    static FILES: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

/// Capture external file paths consulted by Veryl lowering, including its workers.
/// Includes absent lookup candidates, so a new file taking precedence invalidates
/// a cached compilation. Nested captures are independent; panics restore the
/// enclosing capture. Source files must be tracked separately by the caller.
pub fn capture_file_dependencies<T>(compile: impl FnOnce() -> T) -> (T, Vec<PathBuf>) {
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
    FILES.with(|files| {
        if let Some(files) = files.borrow().as_ref() {
            files.lock().unwrap().push(path.to_path_buf());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert_eq!(inner, [PathBuf::from("inner")]);
            record(PathBuf::from("outer").as_path());
        });
        assert_eq!(outer, [PathBuf::from("outer")]);
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
        assert_eq!(outer, [PathBuf::from("outer")]);
    }
}
