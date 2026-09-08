//! Executable-memory ownership for emitted AArch64 functions.

use dynasmrt::mmap::MutableBuffer;
use dynasmrt::{AssemblyOffset, ExecutableBuffer};
use std::{io::Write, sync::Mutex};

static PERF_MAP_INITIALIZED: Mutex<bool> = Mutex::new(false);

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
        perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols_profiled(code, name, &[], perf_map)
    }

    pub fn new_named_with_symbols(
        code: &[u8],
        name: &str,
        symbols: &[JitSymbol],
    ) -> Result<Self, std::io::Error> {
        Self::new_named_with_symbols_profiled(code, name, symbols, false)
    }

    pub fn new_named_with_symbols_profiled(
        code: &[u8],
        name: &str,
        symbols: &[JitSymbol],
        perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        let mut mutable = MutableBuffer::new(code.len().max(1))?;
        mutable.set_len(code.len().max(1));
        mutable[..code.len()].copy_from_slice(code);
        let buffer = mutable.make_exec()?;
        dynasmrt::cache_control::prepare_for_execution(&buffer);
        let fn_ptr = unsafe {
            std::mem::transmute::<*const u8, unsafe extern "C" fn(*mut u8) -> i64>(
                buffer.ptr(AssemblyOffset(0)),
            )
        };
        if perf_map {
            write_perf_map_entries(buffer.as_ptr() as usize, buffer.len(), name, symbols)?;
        }
        Ok(Self { buffer, fn_ptr })
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

fn write_perf_map_entries(
    addr: usize,
    size: usize,
    name: &str,
    symbols: &[JitSymbol],
) -> Result<(), std::io::Error> {
    let path = format!("/tmp/perf-{}.map", std::process::id());
    // A container may reuse a PID while its previous perf map is still in /tmp.
    // Replace that map once, then append subsequent functions under the lock.
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
    let sanitize = |name: &str| name.replace(['\n', '\r', '\t'], "_");
    if symbols.is_empty() {
        writeln!(file, "{addr:x} {size:x} {}", sanitize(name))?;
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
                sanitize(&symbol.name)
            )?;
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn profiles_loaded_images_and_clips_block_symbols() {
        // Loading AArch64 code is also safe on a cross-codegen host; this test
        // checks its address map without executing the instructions.
        let code = [0xc0, 0x03, 0x5f, 0xd6]; // ret
        let first = JitCode::new_named_profiled(&code, "arm64\tfirst", true).unwrap();
        let second = JitCode::new_named_with_symbols_profiled(
            &code,
            "unused_function_name",
            &[
                JitSymbol {
                    offset: 0,
                    size: 16,
                    name: "arm64\nblock".into(),
                },
                JitSymbol {
                    offset: 4,
                    size: 4,
                    name: "outside_image".into(),
                },
                JitSymbol {
                    offset: 0,
                    size: 0,
                    name: "empty_symbol".into(),
                },
            ],
            true,
        )
        .unwrap();
        let map = std::fs::read_to_string(format!("/tmp/perf-{}.map", std::process::id())).unwrap();
        assert!(
            map.lines()
                .any(|line| line == format!("{:x} 4 arm64_first", first.buffer.as_ptr() as usize))
        );
        assert!(
            map.lines()
                .any(|line| line == format!("{:x} 4 arm64_block", second.buffer.as_ptr() as usize))
        );
        assert!(!map.contains("outside_image"));
        assert!(!map.contains("empty_symbol"));
        assert!(!map.contains("unused_function_name"));
    }
}
