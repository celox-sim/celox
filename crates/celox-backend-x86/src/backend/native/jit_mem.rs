//! JIT memory: load emitted machine code into executable memory and call it.

use std::{any::Any, io::Write, sync::Arc};

#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: process-wide perf-map state; simulators built on different caller threads append to the same /tmp/perf-<pid>.map, which must be truncated exactly once"
)]
use std::sync::Mutex;

use memmap2::{Mmap, MmapMut};

#[expect(
    clippy::disallowed_types,
    reason = "cross-thread: process-wide perf-map state; simulators built on different caller threads append to the same /tmp/perf-<pid>.map, which must be truncated exactly once"
)]
static PERF_MAP_INITIALIZED: Mutex<bool> = Mutex::new(false);

#[cfg(target_arch = "x86_64")]
type JitFn = unsafe extern "sysv64" fn(*mut u8) -> i64;
// Cross-codegen never executes the generated image in this process. Use a
// target-supported placeholder ABI so the image container can still be built.
#[cfg(not(target_arch = "x86_64"))]
type JitFn = unsafe extern "C" fn(*mut u8) -> i64;

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
    _mmap: Option<Mmap>,
    _external_owner: Option<Arc<dyn Any + Send + Sync>>,
    code_len: usize,
    // Keep the retained allocation independent of the public, mutable entry.
    // A function pointer preserves JitCode's Send/Sync traits without a raw
    // pointer Send/Sync implementation.
    image_base: JitFn,
    pub fn_ptr: JitFn,
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
        // Align the code itself, not just interior pages, so the hot entry
        // region of a large image can use transparent huge pages too. The
        // unused prefix/suffix stays reserved but is never touched.
        let code_len = code.len().max(1);
        #[cfg(target_os = "linux")]
        let alignment = if code_len >= 2 * 1024 * 1024 {
            2 * 1024 * 1024
        } else {
            1
        };
        #[cfg(not(target_os = "linux"))]
        let alignment = 1;
        // The final code byte also needs a complete huge page in the mapping.
        // Export only code_len bytes, excluding this allocation padding.
        let allocation_len = code_len
            .checked_add(alignment - 1)
            .map(|length| length & !(alignment - 1))
            .and_then(|length| length.checked_add(alignment - 1))
            .ok_or_else(|| std::io::Error::other("JIT image allocation size overflow"))?;
        let mut mmap = MmapMut::map_anon(allocation_len)?;
        let prefix = mmap.as_ptr().align_offset(alignment);
        #[cfg(target_os = "linux")]
        if alignment > 1 {
            // Advice is optional: unsupported or unavailable huge pages use
            // ordinary anonymous pages with exactly the same image bytes.
            let _ = mmap.advise(memmap2::Advice::HugePage);
        }
        mmap[prefix..prefix + code.len()].copy_from_slice(code);
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        if alignment > 1 {
            // Fault-time huge-page advice can leave ordinary pages behind.
            // Collapse once before publishing the immutable executable image;
            // older kernels and allocation failures retain the original pages.
            // Safety: this aligned, rounded range lies wholly in our writable
            // anonymous mapping. MADV_COLLAPSE preserves its contents.
            let _ = unsafe {
                libc::madvise(
                    mmap.as_mut_ptr().add(prefix).cast(),
                    allocation_len - (alignment - 1),
                    libc::MADV_COLLAPSE,
                )
            };
        }
        let mmap = mmap.make_exec()?;
        // Safety: prefix leaves code_len accessible bytes inside the mapping.
        let image_base = unsafe { mmap.as_ptr().add(prefix) };
        // Safety: we just wrote valid x86-64 code into executable memory.
        let fn_ptr: JitFn = unsafe { std::mem::transmute(image_base) };

        if perf_map {
            write_perf_map_entries(image_base as usize, code_len, name, symbols)?;
        }

        Ok(Self {
            _mmap: Some(mmap),
            _external_owner: None,
            code_len,
            image_base: fn_ptr,
            fn_ptr,
        })
    }

    /// Retain an existing immutable executable region without copying it.
    ///
    /// # Safety
    /// `code` must point to `len` readable, executable bytes of valid target
    /// code. The bytes must remain immutable and accessible while `owner` is
    /// alive, and the entry must follow this backend's function ABI.
    pub unsafe fn from_executable_owner(
        owner: Arc<dyn Any + Send + Sync>,
        code: *const u8,
        len: usize,
        name: &str,
        symbols: &[JitSymbol],
        perf_map: bool,
    ) -> Result<Self, std::io::Error> {
        if code.is_null() || len == 0 || len > isize::MAX as usize {
            return Err(std::io::Error::other("invalid executable code region"));
        }
        if perf_map {
            write_perf_map_entries(code as usize, len, name, symbols)?;
        }
        // Safety: the caller supplies a live region with this entry ABI.
        let fn_ptr: JitFn = unsafe { std::mem::transmute(code) };
        Ok(Self {
            _mmap: None,
            _external_owner: Some(owner),
            code_len: len,
            image_base: fn_ptr,
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
        (offset < self.code_len).then(|| unsafe { (self.image_base as *const u8).add(offset) })
    }

    /// Bytes of the executable image, including alignment padding and constant
    /// tables. This is the exact image that can later be copied into an AOT
    /// container.
    pub fn image(&self) -> &[u8] {
        // Safety: either owned mapping or external owner keeps this immutable
        // executable region alive for the lifetime of this borrow.
        unsafe { std::slice::from_raw_parts(self.image_base as *const u8, self.code_len) }
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

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn large_image_alignment_preserves_entries_and_exact_image_bytes() {
        const HUGE_PAGE: usize = 2 * 1024 * 1024;
        let mut bytes = vec![0xcc; HUGE_PAGE + 17];
        bytes[..6].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
        let last = bytes.len() - 6;
        bytes[last..].copy_from_slice(&[0xb8, 7, 0, 0, 0, 0xc3]);
        let mut code = JitCode::new(&bytes).unwrap();
        assert_eq!(code.image().as_ptr() as usize % HUGE_PAGE, 0);
        let mapping = code._mmap.as_ref().unwrap();
        let mapped_end = mapping.as_ptr() as usize + mapping.len();
        let rounded_image_end =
            (code.image().as_ptr() as usize + bytes.len()).next_multiple_of(HUGE_PAGE);
        assert!(rounded_image_end <= mapped_end);
        assert_eq!(code.image(), bytes);
        assert_eq!(unsafe { code.call(&mut []) }, 42);
        assert!(code.entry_ptr(bytes.len()).is_none());
        // Relocating the public entry must not change the retained image base.
        code.fn_ptr =
            unsafe { std::mem::transmute::<*const u8, JitFn>(code.entry_ptr(last).unwrap()) };
        assert_eq!(unsafe { code.call(&mut []) }, 7);
        assert_eq!(code.image(), bytes);
    }

    #[test]
    fn external_executable_owner_survives_until_last_code_user() {
        let bytes = [0xb8, 42, 0, 0, 0, 0xc3];
        let owner = Arc::new(JitCode::new(&bytes).unwrap());
        let weak = Arc::downgrade(&owner);
        // Safety: the source is an immutable executable JitCode and the clone
        // retained by this new region owns its complete backing allocation.
        let borrowed = unsafe {
            JitCode::from_executable_owner(
                owner.clone(),
                owner.image().as_ptr(),
                owner.image().len(),
                "owned_test_code",
                &[],
                false,
            )
        }
        .unwrap();
        drop(owner);
        assert!(weak.upgrade().is_some());
        assert_eq!(borrowed.image(), bytes);
        assert!(borrowed.entry_ptr(bytes.len()).is_none());
        assert_eq!(unsafe { borrowed.call(&mut []) }, 42);
        drop(borrowed);
        assert!(weak.upgrade().is_none());
    }

    fn assert_image_survives_entry_swap(mut code: JitCode, bytes: &[u8]) {
        let base = code.image().as_ptr();
        // Use an allocation of a different size, and release it after swapping
        // the public entries. Neither image accessor may follow the new entry.
        let mut other = JitCode::new(&[0xc3]).unwrap();
        std::mem::swap(&mut code.fn_ptr, &mut other.fn_ptr);
        assert_eq!(code.entry_ptr(0), Some(base));
        drop(other);
        assert_eq!(code.image(), bytes);
        assert_eq!(
            code.entry_ptr(bytes.len() - 1),
            Some(unsafe { base.add(bytes.len() - 1) })
        );
        assert!(code.entry_ptr(bytes.len()).is_none());
        // Execute through the retained image entry, not the swapped fn_ptr.
        let entry: JitFn = unsafe { std::mem::transmute(code.entry_ptr(0).unwrap()) };
        assert_eq!(unsafe { entry(std::ptr::null_mut()) }, 42);
    }

    #[test]
    fn owned_image_is_independent_of_mutable_entry() {
        let bytes = [0xb8, 42, 0, 0, 0, 0xc3];
        assert_image_survives_entry_swap(JitCode::new(&bytes).unwrap(), &bytes);
    }

    #[test]
    fn external_image_is_independent_of_mutable_entry() {
        let bytes = [0xb8, 42, 0, 0, 0, 0xc3];
        let owner = Arc::new(JitCode::new(&bytes).unwrap());
        // Safety: owner retains the immutable executable allocation with the
        // required entry ABI until the external JitCode is dropped.
        let code = unsafe {
            JitCode::from_executable_owner(
                owner.clone(),
                owner.image().as_ptr(),
                owner.image().len(),
                "entry_swap_test",
                &[],
                false,
            )
        }
        .unwrap();
        drop(owner);
        assert_image_survives_entry_swap(code, &bytes);
    }

    #[test]
    fn large_images_preserve_bytes_bounds_and_entry_points() {
        for size in [4096, 2 * 1024 * 1024, 6 * 1024 * 1024 + 37] {
            let mut bytes = vec![0x90; size];
            // mov eax, 7; ret, and a separate entry returning 42.
            bytes[..6].copy_from_slice(&[0xb8, 7, 0, 0, 0, 0xc3]);
            let second = size - 6;
            bytes[second..].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
            let code = JitCode::new(&bytes).unwrap();
            assert_eq!(code.image(), bytes);
            assert!(code.entry_ptr(size).is_none());
            assert!(code.entry_ptr(usize::MAX).is_none());
            let mut state = [11, 22];
            assert_eq!(unsafe { code.call(&mut state) }, 7);
            let second: JitFn = unsafe { std::mem::transmute(code.entry_ptr(second).unwrap()) };
            assert_eq!(unsafe { second(state.as_mut_ptr()) }, 42);
            assert_eq!(state, [11, 22]);
        }
    }
}
