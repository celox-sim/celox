//! JIT memory: load emitted machine code into executable memory and call it.

use std::{io::Write, sync::Mutex};

use memmap2::{Mmap, MmapMut};

static PERF_MAP_INITIALIZED: Mutex<bool> = Mutex::new(false);

/// Optional subrange symbol for Linux perf JIT maps.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub fn new_named(code: &[u8], name: &str) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols(code, name, &[])
    }

    /// Load named machine code and optionally emit Linux perf-map entries.
    pub fn new_named_profiled(
        code: &[u8],
        name: &str,
        perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols_profiled(code, name, &[], perf_map)
    }

    /// Load named machine code bytes with optional subrange symbols.
    pub fn new_named_with_symbols(
        code: &[u8],
        name: &str,
        symbols: &[JitSymbol],
    ) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols_profiled(code, name, symbols, false)
    }

    /// Load named machine code with optional subrange symbols and perf maps.
    pub fn new_named_with_symbols_profiled(
        code: &[u8],
        name: &str,
        symbols: &[JitSymbol],
        perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        // Allocate writable memory, copy code, then make executable
        let mut mmap = MmapMut::map_anon(code.len().max(1))?;
        mmap[..code.len()].copy_from_slice(code);
        let mmap = mmap.make_exec()?;

        // Safety: we just wrote valid x86-64 code into the mmap.
        let fn_ptr: unsafe extern "sysv64" fn(*mut u8) -> i64 =
            unsafe { std::mem::transmute(mmap.as_ptr()) };

        if perf_map {
            write_perf_map_entries(mmap.as_ptr() as usize, code.len().max(1), name, symbols)?;
        }

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

    /// Return a pointer to an entry inside this executable image.
    ///
    /// Native program images contain several independently emitted functions.
    /// Every function is position-independent as long as its complete code and
    /// trailing constant tables are copied together, so the runtime can retain
    /// entry offsets and resolve them after placing the combined image.
    pub fn entry_ptr(&self, offset: usize) -> Option<*const u8> {
        (offset < self._mmap.len()).then(|| unsafe { self._mmap.as_ptr().add(offset) })
    }

    /// Bytes of the executable image, including alignment padding and constant
    /// tables. This is the exact image that can later be copied into an AOT
    /// container.
    pub fn image(&self) -> &[u8] {
        &self._mmap
    }
}

fn write_perf_map_entries(
    addr: usize,
    size: usize,
    name: &str,
    symbols: &[JitSymbol],
) -> Result<(), std::io::Error> {
    let path = format!("/tmp/perf-{}.map", std::process::id());
    // Containerized runs can reuse a PID while the previous process's map is
    // still present in /tmp.  Retaining those entries makes perf resolve an
    // address to an unrelated block from an older compilation.  Truncate once
    // per process, then append the remaining native functions to the same map.
    let mut initialized = PERF_MAP_INITIALIZED
        .lock()
        .map_err(|_| std::io::Error::other("perf map initialization lock was poisoned"))?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true);
    if *initialized {
        options.append(true);
    } else {
        options.truncate(true);
    }
    let mut file = options.open(path)?;
    *initialized = true;
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
