//! Read-only mapping of the executing Linux binary, for the attached-image path.
use std::{fs::File, io, os::fd::AsRawFd, ptr::NonNull};

pub(super) struct ExecutableMapping {
    address: NonNull<u8>,
    len: usize,
}

// Safety: the executing inode cannot be written while it is running. This
// owner only changes page permissions from read-only to read/execute, never
// makes bytes mutable, and unmaps only after the last Arc owner/borrow ends.
unsafe impl Send for ExecutableMapping {}
unsafe impl Sync for ExecutableMapping {}

impl ExecutableMapping {
    pub(super) fn current() -> io::Result<Self> {
        // Open the executing inode, even after its pathname has been replaced.
        let file = File::open("/proc/self/exe")?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::other("executable mapping exceeds address space"))?;
        if len == 0 || len > isize::MAX as usize {
            return Err(io::Error::other("invalid executable mapping length"));
        }
        // Safety: this is the executing inode, not an arbitrary user file.
        // Linux rejects writable opens of executing binaries (ETXTBSY) and
        // rejects execution while writable opens exist. The mapping is never
        // writable, and remains alive until this owner is dropped.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(address) = NonNull::new(address.cast::<u8>()) else {
            // A mapping at address zero cannot back a Rust slice.
            unsafe {
                libc::munmap(address, len);
            }
            return Err(io::Error::other("executable mapped at null address"));
        };
        Ok(Self { address, len })
    }

    #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
    pub(super) fn make_executable(&self, range: std::ops::Range<usize>) -> io::Result<()> {
        if range.is_empty() || range.end > self.len {
            return Err(io::Error::other("invalid executable code range"));
        }
        // Safety: sysconf has no pointer arguments and does not mutate memory.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page)
            .ok()
            .filter(|&page| page != 0)
            .ok_or_else(|| io::Error::other("cannot determine page size"))?;
        let start = range.start / page * page;
        // mprotect rounds the end up to the final mapped page itself.
        // Safety: the checked subrange belongs to this live mapping. Read
        // permission is retained; no writable alias is introduced.
        let result = unsafe {
            libc::mprotect(
                self.address.as_ptr().add(start).cast(),
                range.end - start,
                libc::PROT_READ | libc::PROT_EXEC,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        // Safety: this owner retains an immutable mapping of the checked size;
        // the returned slice cannot outlive it.
        unsafe { std::slice::from_raw_parts(self.address.as_ptr(), self.len) }
    }
}

impl Drop for ExecutableMapping {
    fn drop(&mut self) {
        // Safety: this owner alone unmaps this region after all its borrows end.
        unsafe {
            libc::munmap(self.address.as_ptr().cast(), self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};

    #[test]
    fn executing_inode_mapping_matches_file_boundaries() {
        let mapping = ExecutableMapping::current().unwrap();
        let mut file = File::open("/proc/self/exe").unwrap();
        assert_eq!(
            mapping.as_bytes().len() as u64,
            file.metadata().unwrap().len()
        );
        // Read only two small spans, independently of the test binary's size.
        let mut prefix = [0u8; 16];
        file.read_exact(&mut prefix).unwrap();
        assert_eq!(&mapping.as_bytes()[..16], &prefix);
        file.seek(SeekFrom::End(-16)).unwrap();
        let mut suffix = [0u8; 16];
        file.read_exact(&mut suffix).unwrap();
        assert_eq!(&mapping.as_bytes()[mapping.len - 16..], &suffix);
    }
}
