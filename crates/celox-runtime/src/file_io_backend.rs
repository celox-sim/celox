//! Host-supplied file I/O. Handles here are private to a backend; HDL descriptor
//! allocation and input bookkeeping remain in `FileIo`.
use std::{
    collections::BTreeMap,
    fmt::Debug,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};

/// Validated access mode passed to a host filesystem.
#[derive(Debug, Clone, Copy)]
pub struct FileMode {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub create: bool,
    pub truncate: bool,
}

impl FileMode {
    pub(crate) fn parse(mode: &str) -> Option<Self> {
        let (read, write, append, create, truncate) = match mode {
            "r" | "rb" => (true, false, false, false, false),
            "w" | "wb" => (false, true, false, true, true),
            "a" | "ab" => (false, true, true, true, false),
            "r+" | "r+b" | "rb+" => (true, true, false, false, false),
            "w+" | "w+b" | "wb+" => (true, true, false, true, true),
            "a+" | "a+b" | "ab+" => (true, true, true, true, false),
            _ => return None,
        };
        Some(Self {
            read,
            write,
            append,
            create,
            truncate,
        })
    }
}

/// I/O boundary implemented by an embedding host.
///
/// Each instance owns its handle namespace. `snapshot` records all backend
/// state needed to resume, including open handles, positions and shared-file
/// relationships. The host defines how contents and external effects are
/// retained (for example by versioning, recording, or filesystem snapshots).
/// Celox neither selects nor assumes a storage strategy.
///
/// `restore` must prepare an independent instance without changing the current
/// instance or externally visible state, including on failure. Returned handles
/// must have the same identities as in the snapshot. Dropping/closing the old
/// instance must not invalidate the new one. This permits Celox to validate and
/// prepare every resource before committing the restored simulation state.
/// Hosts may leave the snapshot methods at their defaults. This places no
/// restriction on core checkpoints: only a request to snapshot this handler
/// itself requires these methods.
/// Stdout bytes remain Celox runtime events; they are not withdrawn on restore.
pub trait FileSystem: Debug + Send {
    fn open(&mut self, path: &Path, mode: FileMode) -> io::Result<u64>;
    /// Flush and release a handle, including when flushing reports an error.
    fn close(&mut self, handle: u64) -> io::Result<()>;
    fn read(&mut self, handle: u64, buffer: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, handle: u64, buffer: &[u8]) -> io::Result<usize>;
    fn seek(&mut self, handle: u64, position: SeekFrom) -> io::Result<u64>;
    fn flush(&mut self, handle: u64) -> io::Result<()>;
    fn read_stdin(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(unsupported())
    }
    fn write_stderr(&mut self, _buffer: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }
    fn flush_stdout(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn flush_stderr(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Stable, host-defined format identity, including a version. A restore
    /// rejects a different identity before calling the backend.
    fn snapshot_format(&self) -> &str {
        ""
    }
    /// Capture without consuming input, advancing positions or changing contents.
    fn snapshot(&self) -> io::Result<Vec<u8>> {
        Err(unsupported())
    }
    fn restore(&self, _state: &[u8]) -> io::Result<Box<dyn FileSystem>> {
        Err(unsupported())
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "file I/O backend does not support this operation",
    )
}

/// Creates a fresh I/O namespace for each simulator, including compiler-created
/// shadow instances. Factories must not share mutable stream positions.
#[derive(Clone)]
pub struct FileSystemFactory(Arc<dyn Fn() -> Box<dyn FileSystem> + Send + Sync>);
impl FileSystemFactory {
    pub fn new(factory: impl Fn() -> Box<dyn FileSystem> + Send + Sync + 'static) -> Self {
        Self(Arc::new(factory))
    }
    pub fn create(&self) -> Box<dyn FileSystem> {
        (self.0)()
    }
}
impl Default for FileSystemFactory {
    fn default() -> Self {
        Self::new(|| Box::<HostFileSystem>::default())
    }
}
impl std::fmt::Debug for FileSystemFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileSystemFactory(..)")
    }
}

/// Default host OS implementation. It deliberately cannot promise rollback of
/// arbitrary files, pipes or standard input and does not implement snapshots.
#[derive(Debug, Default)]
pub struct HostFileSystem {
    files: BTreeMap<u64, File>,
    next: u64,
}
impl HostFileSystem {
    fn file(&mut self, handle: u64) -> io::Result<&mut File> {
        self.files.get_mut(&handle).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "unknown backend file handle")
        })
    }
}
impl FileSystem for HostFileSystem {
    fn open(&mut self, path: &Path, mode: FileMode) -> io::Result<u64> {
        let file = OpenOptions::new()
            .read(mode.read)
            .write(mode.write)
            .append(mode.append)
            .create(mode.create)
            .truncate(mode.truncate)
            .open(path)?;
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("file handle space exhausted"))?;
        let id = self.next;
        self.next = next;
        self.files.insert(id, file);
        Ok(id)
    }
    fn close(&mut self, handle: u64) -> io::Result<()> {
        self.files
            .remove(&handle)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "unknown backend file handle")
            })?
            .flush()
    }
    fn read(&mut self, handle: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.file(handle)?.read(buffer)
    }
    fn write(&mut self, handle: u64, buffer: &[u8]) -> io::Result<usize> {
        self.file(handle)?.write(buffer)
    }
    fn seek(&mut self, handle: u64, position: SeekFrom) -> io::Result<u64> {
        self.file(handle)?.seek(position)
    }
    fn flush(&mut self, handle: u64) -> io::Result<()> {
        self.file(handle)?.flush()
    }
    fn read_stdin(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        io::stdin().lock().read(buffer)
    }
    fn write_stderr(&mut self, buffer: &[u8]) -> io::Result<()> {
        io::stderr().lock().write_all(buffer)
    }
    fn flush_stdout(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()
    }
    fn flush_stderr(&mut self) -> io::Result<()> {
        io::stderr().lock().flush()
    }
}
