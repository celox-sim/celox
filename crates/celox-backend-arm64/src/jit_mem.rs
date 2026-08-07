//! Executable-memory ownership for emitted AArch64 functions.

use dynasmrt::mmap::MutableBuffer;
use dynasmrt::{AssemblyOffset, ExecutableBuffer};

/// Optional subrange symbol retained for parity with the native runtime API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JitSymbol {
    pub offset: usize,
    pub size: usize,
    pub name: String,
}

/// Executable AArch64 code region.
pub struct JitCode {
    buffer: ExecutableBuffer,
    pub fn_ptr: unsafe extern "C" fn(*mut u8) -> i64,
}

impl JitCode {
    pub fn new(code: &[u8]) -> Result<Self, std::io::Error> {
        Self::new_named(code, "celox_arm64_jit")
    }

    pub fn new_named(code: &[u8], name: &str) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols(code, name, &[])
    }

    pub fn new_named_profiled(
        code: &[u8],
        name: &str,
        _perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        Self::new_named(code, name)
    }

    pub fn new_named_with_symbols(
        code: &[u8],
        _name: &str,
        _symbols: &[JitSymbol],
    ) -> Result<Self, std::io::Error> {
        let mut mutable = MutableBuffer::new(code.len().max(1))?;
        mutable.set_len(code.len().max(1));
        mutable[..code.len()].copy_from_slice(code);
        let buffer = mutable.make_exec()?;
        dynasmrt::cache_control::prepare_for_execution(&buffer);
        let fn_ptr = unsafe { std::mem::transmute(buffer.ptr(AssemblyOffset(0))) };
        Ok(Self { buffer, fn_ptr })
    }

    pub fn new_named_with_symbols_profiled(
        code: &[u8],
        name: &str,
        symbols: &[JitSymbol],
        _perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols(code, name, symbols)
    }

    /// Execute standalone code with a private spill arena following state.
    ///
    /// # Safety
    /// The emitted function must follow the backend ABI and may only access
    /// the provided state and arena allocation.
    pub unsafe fn call(&self, state: &mut [u8]) -> i64 {
        const STANDALONE_ARENA_BYTES: usize = 1024 * 1024;
        let total = state
            .len()
            .checked_add(STANDALONE_ARENA_BYTES)
            .expect("standalone ARM64 state size overflow");
        let mut owned = vec![0u64; total.div_ceil(8)];
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(owned.as_mut_ptr().cast::<u8>(), owned.len() * 8)
        };
        bytes[..state.len()].copy_from_slice(state);
        let result = unsafe { (self.fn_ptr)(bytes.as_mut_ptr()) };
        state.copy_from_slice(&bytes[..state.len()]);
        result
    }

    pub fn code(&self) -> &[u8] {
        &self.buffer
    }

    /// Return a pointer to an entry inside this executable image.
    ///
    /// AArch64 functions are emitted with their internal branches already
    /// resolved. Keeping each function blob intact therefore permits several
    /// functions to be copied into one image and addressed by offset.
    pub fn entry_ptr(&self, offset: usize) -> Option<*const u8> {
        (offset < self.buffer.len()).then(|| self.buffer.ptr(AssemblyOffset(offset)))
    }

    /// Bytes of the executable image, including alignment padding and literal
    /// data. This is the exact image that can later be copied into an AOT
    /// container.
    pub fn image(&self) -> &[u8] {
        &self.buffer
    }
}
