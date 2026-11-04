//! Reusable storage borrowed by one invocation of the native scheduler.
//!
//! Generated code reads this C-layout header directly. Only capacity growth
//! calls the host; queue operations and snapshots stay in generated code.
use crate::RuntimeErrorCode;

#[repr(C)]
pub(crate) struct Header {
    pub nba: super::native_nba::Header,
    pub slot_observer: u64,
    pub host_values: *mut u64,
    pub host_words: u64,
    pub observers: *mut u64,
    pub observer_initial: u64,
    pub observer_evaluating: u64,
    pub records: *mut u64,
    pub record_capacity: u64,
    pub images: *mut u64,
    pub image_capacity: u64,
    pub record_len: u64,
    pub image_len: u64,
    pub phase: u64,
    pub waves: u64,
    pub wave_limit: u64,
    pub guard_time: u64,
    pub guard_valid: u64,
    pub passes: u64,
    pub work: u64,
    pub returns: u64,
    pub work_limit: u64,
    pub return_limit: u64,
    pub recent_next: u64,
    pub recent: [u64; 64],
}

impl Default for Header {
    fn default() -> Self {
        Self {
            nba: Default::default(),
            slot_observer: 0,
            host_values: std::ptr::null_mut(),
            host_words: 0,
            observers: std::ptr::null_mut(),
            observer_initial: 1,
            observer_evaluating: 0,
            records: std::ptr::null_mut(),
            record_capacity: 0,
            images: std::ptr::null_mut(),
            image_capacity: 0,
            record_len: 0,
            image_len: 0,
            phase: 0,
            waves: 0,
            wave_limit: u64::MAX,
            guard_time: 0,
            guard_valid: 0,
            passes: 0,
            work: 0,
            returns: 0,
            work_limit: u64::MAX,
            return_limit: u64::MAX,
            recent_next: 0,
            recent: [u64::MAX; 64],
        }
    }
}

impl Header {
    pub fn guard_timestamp(&mut self, timestamp: u64) {
        if self.guard_valid == 0 || self.guard_time != timestamp {
            self.guard_valid = 1;
            self.guard_time = timestamp;
            self.passes = 0;
            self.work = 0;
            self.returns = 0;
            self.recent.fill(u64::MAX);
            self.recent_next = 0;
        }
    }
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct Arena {
    pub header: Header,
    pub observers: Vec<u64>,
    pub host_values: Vec<u64>,
    pub nba: super::native_nba::Pool,
    records: Vec<u64>,
    images: Vec<u64>,
}

// These pointers borrow allocations owned by this same object. They
// are accessed only by its exclusive owner or a synchronous generated call.
unsafe impl Send for Arena {}
unsafe impl Sync for Arena {}

impl Arena {
    pub fn reserve(&mut self, images: bool, words: u64) -> Result<(), RuntimeErrorCode> {
        let words = usize::try_from(words).map_err(|_| Self::capacity_error())?;
        // Vec::try_reserve checks the byte-size multiplication and allocation.
        let buffer = if images {
            &mut self.images
        } else {
            &mut self.records
        };
        if words > buffer.len() {
            let capacity = words.max(buffer.len().saturating_mul(2)).max(256);
            buffer
                .try_reserve(capacity - buffer.len())
                .map_err(|_| Self::capacity_error())?;
            buffer.resize(capacity, 0);
        }
        let (pointer, capacity) = if images {
            (&mut self.header.images, &mut self.header.image_capacity)
        } else {
            (&mut self.header.records, &mut self.header.record_capacity)
        };
        *pointer = buffer.as_mut_ptr();
        *capacity = buffer.len() as u64;
        Ok(())
    }

    fn capacity_error() -> RuntimeErrorCode {
        RuntimeErrorCode::Runtime {
            message: "native scheduler arena allocation failed".into(),
            signals: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_arena_retains_storage_and_preserves_records_when_growing() {
        let mut arena = Arena::default();
        for images in [false, true] {
            arena.reserve(images, 3).unwrap();
            let pointer = if images {
                arena.header.images
            } else {
                arena.header.records
            };
            // SAFETY: reserve supplies at least three writable words.
            unsafe {
                pointer.add(2).write(0x0123_4567_89ab_cdef);
            }
            arena.reserve(images, 2).unwrap();
            assert_eq!(
                pointer,
                if images {
                    arena.header.images
                } else {
                    arena.header.records
                }
            );
            arena.reserve(images, 1024).unwrap();
            let pointer = if images {
                arena.header.images
            } else {
                arena.header.records
            };
            // SAFETY: the grown allocation preserves the preceding words.
            assert_eq!(unsafe { pointer.add(2).read() }, 0x0123_4567_89ab_cdef);
            assert!(arena.reserve(images, u64::MAX).is_err());
        }
        assert_eq!(std::mem::offset_of!(Arena, header), 0);
    }
}
