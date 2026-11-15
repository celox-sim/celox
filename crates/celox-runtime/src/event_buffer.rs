#[expect(
    clippy::disallowed_types,
    reason = "FFI/JIT: JIT-compiled and interpreted kernels write runtime event words through the raw pointer from as_mut_ptr while the host reads them with atomic loads"
)]
use std::cell::UnsafeCell;
#[expect(
    clippy::disallowed_types,
    reason = "FFI/JIT: JIT-compiled and interpreted kernels write runtime event words through the raw pointer from as_mut_ptr while the host reads them with atomic loads"
)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

pub struct RuntimeEventBuffer {
    #[expect(
        clippy::disallowed_types,
        reason = "FFI/JIT: JIT-compiled and interpreted kernels write runtime event words through the raw pointer from as_mut_ptr while the host reads them with atomic loads"
    )]
    words: Box<[UnsafeCell<u64>]>,
    byte_size: usize,
}

unsafe impl Send for RuntimeEventBuffer {}
unsafe impl Sync for RuntimeEventBuffer {}

impl RuntimeEventBuffer {
    pub fn new(byte_size: usize) -> Self {
        let word_count = byte_size.div_ceil(8);
        #[expect(
            clippy::disallowed_types,
            reason = "FFI/JIT: JIT-compiled and interpreted kernels write runtime event words through the raw pointer from as_mut_ptr while the host reads them with atomic loads"
        )]
        let words = (0..word_count)
            .map(|_| UnsafeCell::new(0u64))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { words, byte_size }
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.words.as_ptr() as *const u8
    }

    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.words.as_ptr() as *mut u8
    }

    pub fn byte_size(&self) -> usize {
        self.byte_size
    }

    pub fn load_atomic_u64(&self, byte_offset: usize, ordering: Ordering) -> u64 {
        assert_eq!(byte_offset % 8, 0);
        assert!(byte_offset + 8 <= self.byte_size);
        let word = byte_offset / 8;
        unsafe {
            #[expect(
                clippy::disallowed_types,
                reason = "FFI/JIT: JIT-compiled and interpreted kernels write runtime event words through the raw pointer from as_mut_ptr while the host reads them with atomic loads"
            )]
            let ptr = self.words[word].get() as *const AtomicU64;
            (*ptr).load(ordering)
        }
    }

    pub fn read_u64(&self, byte_offset: usize) -> u64 {
        assert_eq!(byte_offset % 8, 0);
        assert!(byte_offset + 8 <= self.byte_size);
        let word = byte_offset / 8;
        unsafe { std::ptr::read_volatile(self.words[word].get()) }
    }

    pub fn reset(&self) {
        for word in &self.words {
            unsafe {
                std::ptr::write_volatile(word.get(), 0);
            }
        }
    }
}
