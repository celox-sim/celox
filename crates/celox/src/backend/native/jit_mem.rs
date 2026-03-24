//! JIT memory: load emitted machine code into executable memory and call it.

use memmap2::{Mmap, MmapMut};

/// Executable code region holding JIT-compiled machine code.
/// The code can be called as `fn(*mut u8) -> i64`.
pub struct JitCode {
    _mmap: Mmap,
    pub fn_ptr: unsafe extern "sysv64" fn(*mut u8) -> i64,
}

impl JitCode {
    /// Load machine code bytes into executable memory.
    pub fn new(code: &[u8]) -> Result<Self, std::io::Error> {
        // Allocate writable memory, copy code, then make executable
        let mut mmap = MmapMut::map_anon(code.len().max(1))?;
        mmap[..code.len()].copy_from_slice(code);
        let mmap = mmap.make_exec()?;

        // Safety: we just wrote valid x86-64 code into the mmap.
        let fn_ptr: unsafe extern "sysv64" fn(*mut u8) -> i64 =
            unsafe { std::mem::transmute(mmap.as_ptr()) };

        Ok(Self {
            _mmap: mmap,
            fn_ptr,
        })
    }

    /// Execute the JIT code with the given simulation state buffer.
    /// Returns the status code (0 = success).
    ///
    /// # Safety
    /// The caller must ensure `state` points to a valid simulation state
    /// buffer of sufficient size, and the JIT code is correct.
    pub unsafe fn call(&self, state: &mut [u8]) -> i64 {
        unsafe { (self.fn_ptr)(state.as_mut_ptr()) }
    }
}
