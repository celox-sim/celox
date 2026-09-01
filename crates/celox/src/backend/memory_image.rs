use std::{any::Any, cell::UnsafeCell, sync::Arc};

/// Fixed-capacity storage for one host simulation memory image.
///
/// The allocation is reference counted separately from the backend so an FFI
/// view can keep the bytes alive without retaining compiled code, scheduler
/// state, or background compilation workers.
struct MemoryAllocation {
    words: Box<[UnsafeCell<u64>]>,
}

// SAFETY: `MemoryAllocation` only owns storage. Safe access to its contents is
// mediated by `MemoryImage`, which requires `&self` for reads and `&mut self`
// for writes. Cloned `Arc`s are opaque lifetime leases and expose no accessors.
// FFI callers that receive a raw pointer remain responsible for synchronizing
// external access with simulator execution, as they were before this owner was
// introduced.
unsafe impl Sync for MemoryAllocation {}

impl MemoryAllocation {
    fn zeroed(capacity_words: usize) -> Self {
        let words = std::iter::repeat_with(|| UnsafeCell::new(0u64))
            .take(capacity_words)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { words }
    }

    fn as_ptr(&self) -> *const u64 {
        self.words.as_ptr().cast::<u64>()
    }

    fn as_mut_ptr(&self) -> *mut u64 {
        UnsafeCell::raw_get(self.words.as_ptr())
    }

    fn capacity_words(&self) -> usize {
        self.words.len()
    }
}

/// A logical memory image backed by a fixed-capacity allocation.
///
/// Growing the logical image is permitted only within its allocation. Tiered
/// execution reserves its maximum required capacity before exposing a pointer,
/// ensuring promotion never invalidates external zero-copy views.
pub(crate) struct MemoryImage {
    allocation: Arc<MemoryAllocation>,
    logical_words: usize,
}

impl MemoryImage {
    pub(crate) fn zeroed(logical_words: usize) -> Self {
        Self {
            allocation: Arc::new(MemoryAllocation::zeroed(logical_words)),
            logical_words,
        }
    }

    pub(crate) fn as_ptr(&self) -> *const u64 {
        self.allocation.as_ptr()
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u64 {
        self.allocation.as_mut_ptr()
    }

    #[cfg(test)]
    fn as_slice(&self) -> &[u64] {
        // SAFETY: the allocation contains at least `logical_words` initialized
        // `u64`s and remains alive for the returned borrow.
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.logical_words) }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u64] {
        // SAFETY: `&mut self` provides the backend-side exclusive borrow. Raw
        // FFI users must separately obey the shared-memory synchronization
        // contract documented on `MemoryAllocation`.
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.logical_words) }
    }

    pub(crate) fn len_words(&self) -> usize {
        self.logical_words
    }

    pub(crate) fn capacity_words(&self) -> usize {
        self.allocation.capacity_words()
    }

    /// Reserve a larger fixed allocation before any external owner is issued.
    pub(crate) fn reserve_total(&mut self, total_words: usize) {
        if total_words <= self.capacity_words() {
            return;
        }
        assert_eq!(
            Arc::strong_count(&self.allocation),
            1,
            "cannot move a simulation memory image after exposing an owner"
        );

        let replacement = Arc::new(MemoryAllocation::zeroed(total_words));
        // SAFETY: both allocations are valid for `logical_words` elements,
        // they cannot overlap, and no external lease exists by the assertion.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.allocation.as_ptr(),
                replacement.as_mut_ptr(),
                self.logical_words,
            );
        }
        self.allocation = replacement;
    }

    /// Change the logical length without moving the allocation.
    pub(crate) fn resize_zeroed_within_capacity(&mut self, new_words: usize) {
        assert!(
            new_words <= self.capacity_words(),
            "simulation memory image exceeded its reserved capacity"
        );
        if new_words > self.logical_words {
            // SAFETY: the asserted capacity covers the extension and the
            // destination lies within this allocation.
            unsafe {
                std::ptr::write_bytes(
                    self.allocation.as_mut_ptr().add(self.logical_words),
                    0,
                    new_words - self.logical_words,
                );
            }
        }
        self.logical_words = new_words;
    }

    pub(crate) fn owner(&self) -> Arc<dyn Any + Send + Sync> {
        self.allocation.clone()
    }
}

impl Default for MemoryImage {
    fn default() -> Self {
        Self::zeroed(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_keeps_only_the_allocation_alive() {
        let image = MemoryImage::zeroed(2);
        let allocation = Arc::downgrade(&image.allocation);
        let first = image.owner();
        let second = image.owner();

        drop(image);
        assert!(allocation.upgrade().is_some());
        drop(first);
        assert!(allocation.upgrade().is_some());
        drop(second);
        assert!(allocation.upgrade().is_none());
    }

    #[test]
    fn reserved_growth_preserves_pointer_and_zeroes_extension() {
        let mut image = MemoryImage::zeroed(2);
        image.as_mut_slice().copy_from_slice(&[11, 22]);
        image.reserve_total(4);
        let ptr = image.as_ptr();

        image.resize_zeroed_within_capacity(4);

        assert_eq!(image.as_ptr(), ptr);
        assert_eq!(image.as_slice(), &[11, 22, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "after exposing an owner")]
    fn reserve_rejects_moving_an_exposed_allocation() {
        let mut image = MemoryImage::zeroed(1);
        let _owner = image.owner();
        image.reserve_total(2);
    }

    #[test]
    fn empty_image_has_no_logical_words() {
        let mut image = MemoryImage::default();
        assert!(image.as_slice().is_empty());
        assert!(image.as_mut_slice().is_empty());
    }
}
