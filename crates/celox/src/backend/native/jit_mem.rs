//! JIT memory: load emitted machine code into executable memory and call it.

use std::io::Write;

use memmap2::{Mmap, MmapMut};

/// Optional subrange symbol for Linux perf JIT maps.
pub struct JitSymbol {
    pub offset: usize,
    pub size: usize,
    pub name: String,
}

/// Executable code region holding JIT-compiled machine code.
/// The code can be called as `fn(*mut u8) -> i64`.
pub struct JitCode {
    _mmap: Mmap,
    pub fn_ptr: unsafe extern "sysv64" fn(*mut u8) -> i64,
}

impl JitCode {
    /// Load machine code bytes into executable memory.
    pub fn new(code: &[u8]) -> Result<Self, std::io::Error> {
        Self::new_named(code, "celox_jit")
    }

    /// Load named machine code bytes into executable memory.
    ///
    /// When `CELOX_PERF_MAP=1` is set, this also writes a Linux perf JIT map
    /// entry so `perf report` can attribute samples to generated functions.
    pub fn new_named(code: &[u8], name: &str) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols(code, name, &[])
    }

    /// Load named machine code bytes with optional subrange symbols.
    pub fn new_named_with_symbols(
        code: &[u8],
        name: &str,
        symbols: &[JitSymbol],
    ) -> Result<Self, std::io::Error> {
        // Allocate writable memory, copy code, then make executable
        let mut mmap = MmapMut::map_anon(code.len().max(1))?;
        mmap[..code.len()].copy_from_slice(code);
        let mmap = mmap.make_exec()?;

        // Safety: we just wrote valid x86-64 code into the mmap.
        let fn_ptr: unsafe extern "sysv64" fn(*mut u8) -> i64 =
            unsafe { std::mem::transmute(mmap.as_ptr()) };

        write_perf_map_entries(mmap.as_ptr() as usize, code.len().max(1), name, symbols)?;

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
        // Standalone emitter/ISel tests pass only their semantic state bytes,
        // while native functions use the following memory as a private
        // spill/scratch/save arena. Production NativeBackend calls `fn_ptr`
        // directly with its already-extended per-instance allocation.
        const STANDALONE_ARENA_BYTES: usize = 1024 * 1024;
        let total_bytes = state
            .len()
            .checked_add(STANDALONE_ARENA_BYTES)
            .expect("standalone native state size overflow");
        let mut owned = vec![0u64; total_bytes.div_ceil(8)];
        let owned_bytes = unsafe {
            std::slice::from_raw_parts_mut(owned.as_mut_ptr().cast::<u8>(), owned.len() * 8)
        };
        owned_bytes[..state.len()].copy_from_slice(state);
        let result = unsafe { (self.fn_ptr)(owned_bytes.as_mut_ptr()) };
        state.copy_from_slice(&owned_bytes[..state.len()]);
        result
    }
}

fn write_perf_map_entries(
    addr: usize,
    size: usize,
    name: &str,
    symbols: &[JitSymbol],
) -> Result<(), std::io::Error> {
    if std::env::var_os("CELOX_PERF_MAP").is_none() {
        return Ok(());
    }

    let path = format!("/tmp/perf-{}.map", std::process::id());
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if symbols.is_empty() {
        writeln!(file, "{addr:x} {size:x} {}", sanitize_perf_symbol(name))?;
    } else {
        for symbol in symbols {
            if symbol.size == 0 || symbol.offset >= size {
                continue;
            }
            let symbol_addr = addr + symbol.offset;
            let symbol_size = symbol.size.min(size - symbol.offset);
            writeln!(
                file,
                "{symbol_addr:x} {symbol_size:x} {}",
                sanitize_perf_symbol(&symbol.name)
            )?;
        }
    }
    Ok(())
}

fn sanitize_perf_symbol(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => '_',
            c => c,
        })
        .collect()
}
