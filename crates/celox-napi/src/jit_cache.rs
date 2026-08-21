//! Coalescing cache for expensive JIT builds.
//!
//! A normal lookup cache still permits a cache stampede: several callers can
//! observe the same miss, compile the same design independently, and only then
//! race to publish equivalent machine code. This cache reserves a missing key
//! for one builder while concurrent callers wait for that exact build result.

use fxhash::FxHashMap as HashMap;
use std::{
    collections::hash_map::Entry,
    hash::Hash,
    sync::{Arc, Mutex, OnceLock},
};

type BuildResult<V> = Result<Arc<V>, String>;
type BuildCell<V> = OnceLock<BuildResult<V>>;

/// Process-local cache which permits at most one in-flight build for each key.
pub(crate) struct SingleFlightCache<K, V>
where
    K: Eq + Hash,
{
    entries: Mutex<HashMap<Arc<K>, Arc<BuildCell<V>>>>,
}

/// Result of looking up a cache key.
pub(crate) enum CacheLookup<'a, K, V>
where
    K: Eq + Hash,
{
    /// A completed build was already cached, or an in-flight build completed.
    Ready(Arc<V>),
    /// This caller owns the reservation and must publish the build result.
    Build(BuildPermit<'a, K, V>),
    /// The caller joined an in-flight build which failed.
    Failed(String),
}

/// Reservation for the sole builder of one cache key.
///
/// Dropping an incomplete permit removes the reservation and publishes an
/// error, so an early return or panic cannot leave joined callers blocked.
pub(crate) struct BuildPermit<'a, K, V>
where
    K: Eq + Hash,
{
    cache: &'a SingleFlightCache<K, V>,
    key: Option<Arc<K>>,
    cell: Arc<BuildCell<V>>,
}

impl<K, V> SingleFlightCache<K, V>
where
    K: Eq + Hash,
{
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::default()),
        }
    }

    /// Return a completed value, join its in-flight build, or reserve a miss.
    pub(crate) fn lookup_or_reserve(&self, key: K) -> CacheLookup<'_, K, V> {
        let key = Arc::new(key);
        let (cell, should_build) = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match entries.entry(Arc::clone(&key)) {
                Entry::Occupied(entry) => (Arc::clone(entry.get()), false),
                Entry::Vacant(entry) => {
                    let cell = Arc::new(OnceLock::new());
                    entry.insert(Arc::clone(&cell));
                    (cell, true)
                }
            }
        };

        if should_build {
            CacheLookup::Build(BuildPermit {
                cache: self,
                key: Some(key),
                cell,
            })
        } else {
            match cell.wait().clone() {
                Ok(value) => CacheLookup::Ready(value),
                Err(message) => CacheLookup::Failed(message),
            }
        }
    }

    /// Remove completed and in-flight entries.
    ///
    /// Builders and callers which already joined them keep their old cell
    /// alive through `Arc`. A later build for the same key receives a different
    /// cell, so a stale publisher cannot affect the new cache entry.
    pub(crate) fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    fn publish(
        &self,
        key: &Arc<K>,
        cell: &Arc<BuildCell<V>>,
        result: BuildResult<V>,
    ) {
        if result.is_err() {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let owns_entry = entries
                .get(key)
                .is_some_and(|current| Arc::ptr_eq(current, cell));
            if owns_entry {
                entries.remove(key);
            }
        }

        assert!(
            cell.set(result).is_ok(),
            "single-flight build result published more than once"
        );
    }
}

impl<K, V> BuildPermit<'_, K, V>
where
    K: Eq + Hash,
{
    /// Publish the build result to both the cache and all joined callers.
    pub(crate) fn complete(mut self, result: BuildResult<V>) {
        let key = self
            .key
            .take()
            .expect("single-flight build permit completed more than once");
        self.cache.publish(&key, &self.cell, result);
    }
}

impl<K, V> Drop for BuildPermit<'_, K, V>
where
    K: Eq + Hash,
{
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        self.cache.publish(
            &key,
            &self.cell,
            Err("JIT compilation ended before publishing a cache result".to_string()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    fn get_or_build(
        cache: &SingleFlightCache<u8, usize>,
        key: u8,
        build: impl FnOnce() -> Result<usize, String>,
    ) -> Result<Arc<usize>, String> {
        match cache.lookup_or_reserve(key) {
            CacheLookup::Ready(value) => Ok(value),
            CacheLookup::Failed(message) => Err(message),
            CacheLookup::Build(permit) => {
                let result = build().map(Arc::new);
                permit.complete(result.clone());
                result
            }
        }
    }

    #[test]
    fn coalesces_concurrent_builds() {
        const THREADS: usize = 8;

        let cache = Arc::new(SingleFlightCache::new());
        let build_count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles = (0..THREADS)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let build_count = Arc::clone(&build_count);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    get_or_build(&cache, 7, || {
                        build_count.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(20));
                        Ok(42)
                    })
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();

        let values = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(build_count.load(Ordering::SeqCst), 1);
        assert!(values.iter().all(|value| **value == 42));
        assert!(
            values
                .iter()
                .skip(1)
                .all(|value| Arc::ptr_eq(&values[0], value))
        );
    }

    #[test]
    fn failed_build_is_not_cached() {
        let cache = SingleFlightCache::new();

        assert_eq!(
            get_or_build(&cache, 1, || Err("compile failed".to_string())).unwrap_err(),
            "compile failed"
        );
        assert_eq!(*get_or_build(&cache, 1, || Ok(9)).unwrap(), 9);
        assert_eq!(
            *get_or_build(&cache, 1, || panic!("cached result expected")).unwrap(),
            9
        );
    }

    #[test]
    fn clear_prevents_an_old_build_from_repopulating_the_cache() {
        let cache = SingleFlightCache::new();
        let old = match cache.lookup_or_reserve(3) {
            CacheLookup::Build(permit) => permit,
            CacheLookup::Ready(_) | CacheLookup::Failed(_) => panic!("first lookup must build"),
        };

        cache.clear();

        let new = match cache.lookup_or_reserve(3) {
            CacheLookup::Build(permit) => permit,
            CacheLookup::Ready(_) | CacheLookup::Failed(_) => {
                panic!("lookup after clear must build")
            }
        };
        new.complete(Ok(Arc::new(2)));
        old.complete(Ok(Arc::new(1)));

        match cache.lookup_or_reserve(3) {
            CacheLookup::Ready(value) => assert_eq!(*value, 2),
            CacheLookup::Build(_) | CacheLookup::Failed(_) => {
                panic!("new result must remain cached")
            }
        }
    }
}
