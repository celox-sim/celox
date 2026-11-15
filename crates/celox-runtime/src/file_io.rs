//! SystemVerilog file descriptors and multichannel descriptors (IEEE 1800-2023 21.3).
//!
//! A manager belongs to one simulation. No process-global descriptor table is used.
use crate::file_io_backend::{FileMode, FileSystem, HostFileSystem};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: shares one simulation's file system backend between its descriptor manager and open file handles, all used on that simulation's thread; Mutex only keeps FileIo Send"
)]
use std::sync::Mutex;

use celox_testbench::{DisplayFormatArg, FieldSpec, format_sv_display_arg};

const FD_FLAG: u32 = 0x8000_0000;

#[derive(Debug)]
pub struct FileIo {
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: shares one simulation's file system backend between its descriptor manager and open file handles, all used on that simulation's thread; Mutex only keeps FileIo Send"
    )]
    backend: Arc<Mutex<Box<dyn FileSystem>>>,
    channels: BTreeMap<u32, BackendFile>,
    descriptors: BTreeMap<u32, InputFile>,
    stdin_input: InputState,
    stdin_open: bool,
    stdout_open: bool,
    stderr_open: bool,
    errors: BTreeMap<u32, FileError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FileError {
    code: i32,
    message: String,
}
impl From<&io::Error> for FileError {
    fn from(error: &io::Error) -> Self {
        Self {
            code: error.raw_os_error().filter(|code| *code != 0).unwrap_or(1),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct InputState {
    eof: bool,
    pushed: Option<u8>,
}

#[derive(Debug)]
struct InputFile {
    file: BackendFile,
    readable: bool,
    input: InputState,
}

impl Default for FileIo {
    fn default() -> Self {
        Self::new(Box::<HostFileSystem>::default())
    }
}

impl FileIo {
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: shares one simulation's file system backend between its descriptor manager and open file handles, all used on that simulation's thread; Mutex only keeps FileIo Send"
    )]
    pub fn new(backend: Box<dyn FileSystem>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
            channels: BTreeMap::new(),
            descriptors: BTreeMap::new(),
            stdin_input: InputState::default(),
            stdin_open: true,
            stdout_open: true,
            stderr_open: true,
            errors: BTreeMap::new(),
        }
    }

    /// Capture Celox's bookkeeping and the backend's opaque state. Unsupported
    /// backend capture is retained as a restore error, allowing inspection.
    pub fn snapshot(&self) -> FileIoSnapshot {
        let backend = self.backend.lock().unwrap();
        let state = backend
            .snapshot()
            .and_then(|bytes| {
                if backend.snapshot_format().is_empty() {
                    return Err(io::Error::other(
                        "snapshot backend must declare a format identity",
                    ));
                }
                Ok((backend.snapshot_format().to_owned(), bytes))
            })
            .map_err(|error| error.to_string());
        FileIoSnapshot {
            state,
            channels: self
                .channels
                .iter()
                .map(|(&key, file)| (key, file.id))
                .collect(),
            descriptors: self
                .descriptors
                .iter()
                .map(|(&key, file)| (key, file.file.id, file.readable, file.input.clone()))
                .collect(),
            stdin_input: self.stdin_input.clone(),
            standard_open: [self.stdin_open, self.stdout_open, self.stderr_open],
            errors: self.errors.clone(),
        }
    }

    /// Capture a resumable handler state, reporting backend capture failures.
    /// System-level checkpoint writers use this rather than publishing a
    /// diagnostic snapshot containing an error.
    pub fn try_snapshot(&self) -> io::Result<FileIoSnapshot> {
        let snapshot = self.snapshot();
        snapshot
            .state
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        Ok(snapshot)
    }

    /// Prepare a replacement without changing this manager or its backend.
    pub fn prepare_restore(&self, snapshot: &FileIoSnapshot) -> io::Result<Self> {
        let (format, bytes) = snapshot
            .state
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        let backend = self.backend.lock().unwrap();
        if backend.snapshot_format() != format {
            return Err(io::Error::other(
                "file I/O snapshot backend format mismatch",
            ));
        }
        let restored = backend.restore(bytes)?;
        if restored.snapshot_format() != format {
            return Err(io::Error::other(
                "restored file I/O backend format mismatch",
            ));
        }
        let mut manager = Self::new(restored);
        for &(key, id) in &snapshot.channels {
            manager.channels.insert(
                key,
                BackendFile {
                    id,
                    closed: false,
                    backend: manager.backend.clone(),
                },
            );
        }
        for (key, id, readable, input) in &snapshot.descriptors {
            manager.descriptors.insert(
                *key,
                InputFile {
                    file: BackendFile {
                        id: *id,
                        closed: false,
                        backend: manager.backend.clone(),
                    },
                    readable: *readable,
                    input: input.clone(),
                },
            );
        }
        manager.errors = snapshot.errors.clone();
        manager.stdin_input = snapshot.stdin_input.clone();
        [manager.stdin_open, manager.stdout_open, manager.stderr_open] = snapshot.standard_open;
        Ok(manager)
    }

    /// Open an MCD when mode is absent, otherwise an FD. Failure returns zero,
    /// as specified by 21.3.1. Closed channel numbers are reused.
    pub fn open(&mut self, path: impl AsRef<Path>, mode: Option<&str>) -> u32 {
        match self.open_inner(path.as_ref(), mode) {
            Ok(descriptor) => {
                self.errors.remove(&0);
                self.errors.remove(&descriptor);
                descriptor
            }
            Err(error) => {
                self.errors.insert(0, FileError::from(&error));
                0
            }
        }
    }

    fn open_inner(&mut self, path: &Path, mode: Option<&str>) -> io::Result<u32> {
        let Some(options) = FileMode::parse(mode.unwrap_or("w")) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid file mode",
            ));
        };
        let key = if mode.is_some() {
            let Some(key) = (3..FD_FLAG).find(|key| !self.descriptors.contains_key(key)) else {
                return Err(io::Error::other("file descriptor table is full"));
            };
            key
        } else {
            let Some(key) = (1..31)
                .map(|bit| 1u32 << bit)
                .find(|key| !self.channels.contains_key(key))
            else {
                return Err(io::Error::other("file descriptor table is full"));
            };
            key
        };
        let id = self.backend.lock().unwrap().open(path, options)?;
        let mut file = BackendFile {
            id,
            closed: false,
            backend: self.backend.clone(),
        };
        if let Some(mode) = mode {
            // Table 21-6 places the initial position at EOF for append modes,
            // including the read position of update streams (a+).
            if mode.starts_with('a') {
                file.seek(SeekFrom::End(0))?;
            }
            self.descriptors.insert(
                key,
                InputFile {
                    file,
                    readable: mode.starts_with('r') || mode.contains('+'),
                    input: InputState::default(),
                },
            );
            Ok(key | FD_FLAG)
        } else {
            self.channels.insert(key, file);
            Ok(key)
        }
    }

    /// Close every selected MCD channel, or one FD. Closing a standard stream
    /// changes this simulation only; it never closes the host process stream.
    pub fn close(&mut self, descriptor: u32) -> io::Result<()> {
        self.with_status(descriptor, |manager| manager.close_inner(descriptor))
    }

    fn close_inner(&mut self, descriptor: u32) -> io::Result<()> {
        if descriptor == 0 {
            return Err(invalid_descriptor());
        }
        if descriptor & FD_FLAG != 0 {
            let key = descriptor & !FD_FLAG;
            let standard_open = match key {
                0 => Some(&mut self.stdin_open),
                1 => Some(&mut self.stdout_open),
                2 => Some(&mut self.stderr_open),
                _ => None,
            };
            if let Some(open) = standard_open {
                return if std::mem::replace(open, false) {
                    Ok(())
                } else {
                    Err(invalid_descriptor())
                };
            }
            let Some(file) = self.descriptors.remove(&key) else {
                return Err(invalid_descriptor());
            };
            return file.file.close();
        }
        let mut error = None;
        if descriptor & 1 != 0 && !std::mem::replace(&mut self.stdout_open, false) {
            error = Some(invalid_descriptor());
        }
        for bit in 1..31 {
            let key = 1u32 << bit;
            if descriptor & key != 0 {
                match self.channels.remove(&key) {
                    Some(file) => {
                        if let Err(err) = file.close() {
                            error = Some(err);
                        }
                    }
                    None => error = Some(invalid_descriptor()),
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    /// Write bytes in call order. MCD bit zero and FD stdout are returned to the
    /// caller so they can use the simulator's ordinary output/event interface.
    /// Stderr is written directly. All selected valid channels are attempted.
    pub fn write(&mut self, descriptor: u32, bytes: &[u8]) -> io::Result<bool> {
        self.with_status(descriptor, |manager| manager.write_inner(descriptor, bytes))
    }

    fn write_inner(&mut self, descriptor: u32, bytes: &[u8]) -> io::Result<bool> {
        if descriptor == 0 {
            return Err(invalid_descriptor());
        }
        if descriptor & FD_FLAG != 0 {
            return match descriptor & !FD_FLAG {
                0 => Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "STDIN is not writable",
                )),
                1 if self.stdout_open => Ok(true),
                2 if self.stderr_open => {
                    self.backend.lock().unwrap().write_stderr(bytes)?;
                    Ok(false)
                }
                key => {
                    self.descriptors
                        .get_mut(&key)
                        .ok_or_else(invalid_descriptor)?
                        .file
                        .write_all(bytes)?;
                    Ok(false)
                }
            };
        }
        let mut error = None;
        for bit in 1..31 {
            let key = 1u32 << bit;
            if descriptor & key != 0 {
                let result = self
                    .channels
                    .get_mut(&key)
                    .ok_or_else(invalid_descriptor)
                    .and_then(|file| file.write_all(bytes));
                if let Err(err) = result {
                    error = Some(err);
                }
            }
        }
        if descriptor & 1 != 0 && !self.stdout_open {
            error = Some(invalid_descriptor());
        }
        error.map_or(Ok(descriptor & 1 != 0), Err)
    }

    /// Write all selected channels and report stdout selection independently of
    /// other channel errors, so callers can publish stdout before returning an error.
    pub fn write_with_stdout(&mut self, descriptor: u32, bytes: &[u8]) -> (bool, io::Result<()>) {
        let stdout = self.stdout_open
            && (descriptor == FD_FLAG | 1 || descriptor & FD_FLAG == 0 && descriptor & 1 != 0);
        let result = self.write(descriptor, bytes).map(|_| ());
        (stdout, result)
    }

    /// Read a byte from an FD. EOF is latched only after a read reaches it;
    /// a pushed-back byte is returned before the underlying file is read.
    pub fn read_byte(&mut self, descriptor: u32) -> io::Result<Option<u8>> {
        self.with_status(descriptor, |manager| manager.read_byte_inner(descriptor))
    }

    fn read_byte_inner(&mut self, descriptor: u32) -> io::Result<Option<u8>> {
        if descriptor == FD_FLAG && self.stdin_open {
            return read_input_byte(&mut BackendStdin(&self.backend), &mut self.stdin_input);
        }
        let file = self.readable_file(descriptor)?;
        read_input_byte(&mut file.file, &mut file.input)
    }

    /// Push back one character without changing the file. One byte is
    /// guaranteed, as with C stdio; another unread before a read is rejected.
    pub fn unread_byte(&mut self, descriptor: u32, byte: u8) -> io::Result<()> {
        self.with_status(descriptor, |manager| {
            manager.unread_byte_inner(descriptor, byte)
        })
    }

    fn unread_byte_inner(&mut self, descriptor: u32, byte: u8) -> io::Result<()> {
        let input = if descriptor == FD_FLAG && self.stdin_open {
            &mut self.stdin_input
        } else {
            &mut self.readable_file(descriptor)?.input
        };
        if input.pushed.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file pushback buffer is full",
            ));
        }
        input.pushed = Some(byte);
        input.eof = false;
        Ok(())
    }

    /// Whether a previous read encountered EOF (21.3.8).
    pub fn eof(&mut self, descriptor: u32) -> io::Result<bool> {
        self.with_status(descriptor, |manager| manager.eof_inner(descriptor))
    }

    fn eof_inner(&mut self, descriptor: u32) -> io::Result<bool> {
        if descriptor == FD_FLAG && self.stdin_open {
            return Ok(self.stdin_input.eof);
        }
        Ok(self.readable_file(descriptor)?.input.eof)
    }

    /// Byte position of the next read/write, accounting for pushback.
    pub fn tell(&mut self, descriptor: u32) -> io::Result<u64> {
        self.with_status(descriptor, |manager| manager.tell_inner(descriptor))
    }

    fn tell_inner(&mut self, descriptor: u32) -> io::Result<u64> {
        let file = self.positioned_file(descriptor)?;
        file.file
            .stream_position()?
            .checked_sub(u64::from(file.input.pushed.is_some()))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "pushback precedes beginning of file",
                )
            })
    }

    /// Seek relative to start (0), logical current position (1), or end (2).
    /// Successful positioning clears both pushback and the EOF indicator.
    pub fn seek(&mut self, descriptor: u32, offset: i64, operation: i32) -> io::Result<()> {
        self.with_status(descriptor, |manager| {
            manager.seek_inner(descriptor, offset, operation)
        })
    }

    fn seek_inner(&mut self, descriptor: u32, offset: i64, operation: i32) -> io::Result<()> {
        let file = self.positioned_file(descriptor)?;
        let position = match operation {
            0 => SeekFrom::Start(u64::try_from(offset).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "negative file position")
            })?),
            1 => {
                let current = file
                    .file
                    .stream_position()?
                    .checked_sub(u64::from(file.input.pushed.is_some()))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "pushback precedes beginning of file",
                        )
                    })?;
                let target = current.checked_add_signed(offset).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "file position out of range")
                })?;
                SeekFrom::Start(target)
            }
            2 => SeekFrom::End(offset),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid seek operation",
                ));
            }
        };
        file.file.seek(position)?;
        file.input = InputState::default();
        Ok(())
    }

    /// Reposition to the beginning, clearing EOF and pushback.
    pub fn rewind(&mut self, descriptor: u32) -> io::Result<()> {
        self.seek(descriptor, 0, 0)
    }

    /// Flush one FD/MCD, or all open output files when no descriptor is given.
    /// Attempt every selected channel even if another selected channel fails.
    pub fn flush(&mut self, descriptor: Option<u32>) -> io::Result<()> {
        if let Some(descriptor) = descriptor {
            return self.with_status(descriptor, |manager| manager.flush_inner(Some(descriptor)));
        }
        let descriptors: Vec<_> = self
            .descriptors
            .keys()
            .map(|key| key | FD_FLAG)
            .chain(self.channels.keys().copied())
            .chain(self.stdout_open.then_some(FD_FLAG | 1))
            .chain(self.stderr_open.then_some(FD_FLAG | 2))
            .collect();
        let mut result = Ok(());
        for descriptor in descriptors {
            if let Err(error) = self.flush(Some(descriptor)) {
                result = Err(error);
            }
        }
        result
    }

    fn flush_inner(&mut self, descriptor: Option<u32>) -> io::Result<()> {
        if let Some(descriptor) = descriptor {
            if descriptor == 0 {
                return Err(invalid_descriptor());
            }
            if descriptor & FD_FLAG != 0 {
                return match descriptor & !FD_FLAG {
                    0 if self.stdin_open => Ok(()),
                    1 if self.stdout_open => self.backend.lock().unwrap().flush_stdout(),
                    2 if self.stderr_open => self.backend.lock().unwrap().flush_stderr(),
                    _ => self.positioned_file(descriptor)?.file.flush(),
                };
            }
        }
        let mut error = None;
        let mut attempt = |result: io::Result<()>| {
            if let Err(err) = result {
                error = Some(err);
            }
        };
        if descriptor.is_none() || descriptor.is_some_and(|bits| bits & 1 != 0) {
            if self.stdout_open {
                attempt(self.backend.lock().unwrap().flush_stdout());
            } else if descriptor.is_some() {
                attempt(Err(invalid_descriptor()));
            }
        }
        if descriptor.is_none() && self.stderr_open {
            attempt(self.backend.lock().unwrap().flush_stderr());
        }
        for bit in 1..31 {
            let key = 1u32 << bit;
            if descriptor.is_none() || descriptor.is_some_and(|bits| bits & key != 0) {
                match self.channels.get_mut(&key) {
                    Some(file) => attempt(file.flush()),
                    None if descriptor.is_some() => attempt(Err(invalid_descriptor())),
                    None => {}
                }
            }
        }
        if descriptor.is_none() {
            for file in self.descriptors.values_mut() {
                attempt(file.file.flush());
            }
        }
        error.map_or(Ok(()), Err)
    }

    fn with_status<T>(
        &mut self,
        descriptor: u32,
        action: impl FnOnce(&mut Self) -> io::Result<T>,
    ) -> io::Result<T> {
        let result = action(self);
        match &result {
            Ok(_) => {
                self.errors.remove(&descriptor);
            }
            Err(error) if self.valid_fd(descriptor) => {
                self.errors.insert(descriptor, FileError::from(error));
            }
            Err(_) => {
                self.errors.remove(&descriptor);
            }
        }
        result
    }

    /// Latest I/O status for an FD (21.3.7). Querying does not clear it.
    /// Codes and messages depend on the host; successful operations clear both.
    pub fn error(&self, descriptor: u32) -> (i32, String) {
        if let Some(error) = self.errors.get(&descriptor) {
            return (error.code, error.message.clone());
        }
        if self.valid_fd(descriptor) {
            (0, String::new())
        } else {
            let error = FileError::from(&invalid_descriptor());
            (error.code, error.message)
        }
    }

    fn valid_fd(&self, descriptor: u32) -> bool {
        match descriptor {
            0x8000_0000 => self.stdin_open,
            0x8000_0001 => self.stdout_open,
            0x8000_0002 => self.stderr_open,
            _ => {
                descriptor & FD_FLAG != 0 && self.descriptors.contains_key(&(descriptor & !FD_FLAG))
            }
        }
    }

    fn positioned_file(&mut self, descriptor: u32) -> io::Result<&mut InputFile> {
        if descriptor & FD_FLAG == 0 {
            return Err(invalid_descriptor());
        }
        self.descriptors
            .get_mut(&(descriptor & !FD_FLAG))
            .ok_or_else(invalid_descriptor)
    }

    fn readable_file(&mut self, descriptor: u32) -> io::Result<&mut InputFile> {
        let file = self.positioned_file(descriptor)?;
        if !file.readable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is not open for reading",
            ));
        }
        Ok(file)
    }

    /// Write memory through the injected backend, including its snapshot policy.
    pub fn write_memory_with_backend<'a>(
        &mut self,
        path: impl AsRef<Path>,
        binary: bool,
        words: impl IntoIterator<Item = DisplayFormatArg<'a>>,
    ) -> io::Result<()> {
        let id = self
            .backend
            .lock()
            .unwrap()
            .open(path.as_ref(), FileMode::parse("w").unwrap())?;
        let mut file = BackendFile {
            id,
            closed: false,
            backend: self.backend.clone(),
        };
        write_memory_words(&mut file, binary, words)?;
        file.close()
    }

    /// Write a static unpacked memory snapshot, without address specifiers
    /// (21.5.3). The caller supplies elements in the requested address order.
    pub fn write_memory<'a>(
        path: impl AsRef<Path>,
        binary: bool,
        words: impl IntoIterator<Item = DisplayFormatArg<'a>>,
    ) -> io::Result<()> {
        let mut file = File::create(path)?;
        write_memory_words(&mut file, binary, words)
    }
}

fn write_memory_words<'a>(
    file: &mut impl Write,
    binary: bool,
    words: impl IntoIterator<Item = DisplayFormatArg<'a>>,
) -> io::Result<()> {
    for word in words {
        file.write_all(&format_sv_display_arg(
            &word,
            if binary { 'b' } else { 'h' },
            FieldSpec::default(),
        ))?;
        file.write_all(b"\n")?;
    }
    file.flush()
}

/// Host-owned I/O snapshot, separate from core checkpoints. Backend bytes are opaque; descriptor
/// bookkeeping cannot be edited independently of them.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileIoSnapshot {
    state: Result<(String, Vec<u8>), String>,
    channels: Vec<(u32, u64)>,
    descriptors: Vec<(u32, u64, bool, InputState)>,
    stdin_input: InputState,
    standard_open: [bool; 3],
    #[serde(default)]
    errors: BTreeMap<u32, FileError>,
}

#[derive(Debug)]
struct BackendFile {
    id: u64,
    closed: bool,
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: shares one simulation's file system backend between its descriptor manager and open file handles, all used on that simulation's thread; Mutex only keeps FileIo Send"
    )]
    backend: Arc<Mutex<Box<dyn FileSystem>>>,
}
impl BackendFile {
    fn close(mut self) -> io::Result<()> {
        self.closed = true;
        self.backend.lock().unwrap().close(self.id)
    }
}
impl Read for BackendFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.backend.lock().unwrap().read(self.id, buffer)
    }
}
impl Write for BackendFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.backend.lock().unwrap().write(self.id, buffer)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.backend.lock().unwrap().flush(self.id)
    }
}
impl Seek for BackendFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.backend.lock().unwrap().seek(self.id, position)
    }
}
impl Drop for BackendFile {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let _ = self
            .backend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .close(self.id);
    }
}
struct BackendStdin<'a>(
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: shares one simulation's file system backend between its descriptor manager and open file handles, all used on that simulation's thread; Mutex only keeps FileIo Send"
    )]
    &'a Arc<Mutex<Box<dyn FileSystem>>>,
);
impl Read for BackendStdin<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.lock().unwrap().read_stdin(buffer)
    }
}

fn read_input_byte(reader: &mut impl Read, input: &mut InputState) -> io::Result<Option<u8>> {
    if let Some(byte) = input.pushed.take() {
        return Ok(Some(byte));
    }
    if input.eof {
        return Ok(None);
    }
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                input.eof = true;
                return Ok(None);
            }
            Ok(_) => return Ok(Some(byte[0])),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn invalid_descriptor() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid or closed SystemVerilog file descriptor",
    )
}

#[cfg(test)]
mod tests;
