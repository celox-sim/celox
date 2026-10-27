//! SystemVerilog file descriptors and multichannel descriptors (IEEE 1800-2023 21.3).
//!
//! A manager belongs to one simulation. No process-global descriptor table is used.
use crate::file_io_backend::{FileMode, FileSystem, HostFileSystem};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use celox_testbench::{DisplayFormatArg, FieldSpec, format_sv_display_arg};

const FD_FLAG: u32 = 0x8000_0000;

#[derive(Debug)]
pub struct FileIo {
    backend: Arc<Mutex<Box<dyn FileSystem>>>,
    channels: BTreeMap<u32, BackendFile>,
    descriptors: BTreeMap<u32, InputFile>,
    stdin_input: InputState,
    stdin_open: bool,
    stdout_open: bool,
    stderr_open: bool,
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
    pub fn new(backend: Box<dyn FileSystem>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
            channels: BTreeMap::new(),
            descriptors: BTreeMap::new(),
            stdin_input: InputState::default(),
            stdin_open: true,
            stdout_open: true,
            stderr_open: true,
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
        manager.stdin_input = snapshot.stdin_input.clone();
        [manager.stdin_open, manager.stdout_open, manager.stderr_open] = snapshot.standard_open;
        Ok(manager)
    }

    /// Open an MCD when mode is absent, otherwise an FD. Failure returns zero,
    /// as specified by 21.3.1. Closed channel numbers are reused.
    pub fn open(&mut self, path: impl AsRef<Path>, mode: Option<&str>) -> u32 {
        let Some(options) = FileMode::parse(mode.unwrap_or("w")) else {
            return 0;
        };
        let key = if mode.is_some() {
            let Some(key) = (3..FD_FLAG).find(|key| !self.descriptors.contains_key(key)) else {
                return 0;
            };
            key
        } else {
            let Some(key) = (1..31)
                .map(|bit| 1u32 << bit)
                .find(|key| !self.channels.contains_key(key))
            else {
                return 0;
            };
            key
        };
        let Ok(id) = self.backend.lock().unwrap().open(path.as_ref(), options) else {
            return 0;
        };
        let mut file = BackendFile {
            id,
            closed: false,
            backend: self.backend.clone(),
        };
        if let Some(mode) = mode {
            // Table 21-6 places the initial position at EOF for append modes,
            // including the read position of update streams (a+).
            if mode.starts_with('a') && file.seek(SeekFrom::End(0)).is_err() {
                return 0;
            }
            self.descriptors.insert(
                key,
                InputFile {
                    file,
                    readable: mode.starts_with('r') || mode.contains('+'),
                    input: InputState::default(),
                },
            );
            key | FD_FLAG
        } else {
            self.channels.insert(key, file);
            key
        }
    }

    /// Close every selected MCD channel, or one FD. Closing a standard stream
    /// changes this simulation only; it never closes the host process stream.
    pub fn close(&mut self, descriptor: u32) -> io::Result<()> {
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
        if descriptor == FD_FLAG && self.stdin_open {
            return read_input_byte(&mut BackendStdin(&self.backend), &mut self.stdin_input);
        }
        let file = self.readable_file(descriptor)?;
        read_input_byte(&mut file.file, &mut file.input)
    }

    /// Push back one character without changing the file. One byte is
    /// guaranteed, as with C stdio; another unread before a read is rejected.
    pub fn unread_byte(&mut self, descriptor: u32, byte: u8) -> io::Result<()> {
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
        if descriptor == FD_FLAG && self.stdin_open {
            return Ok(self.stdin_input.eof);
        }
        Ok(self.readable_file(descriptor)?.input.eof)
    }

    /// Byte position of the next read/write, accounting for pushback.
    pub fn tell(&mut self, descriptor: u32) -> io::Result<u64> {
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
}

#[derive(Debug)]
struct BackendFile {
    id: u64,
    closed: bool,
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
struct BackendStdin<'a>(&'a Arc<Mutex<Box<dyn FileSystem>>>);
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
mod tests {
    use super::*;
    use num_bigint::BigUint;

    #[test]
    fn byte_input_tracks_eof_pushback_and_logical_position() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input");
        std::fs::write(&path, [0, 255, b'Z']).unwrap();
        let mut files = FileIo::default();
        let fd = files.open(&path, Some("rb"));
        assert!(!files.eof(fd).unwrap());
        assert_eq!(files.tell(fd).unwrap(), 0);
        assert_eq!(files.read_byte(fd).unwrap(), Some(0));
        assert_eq!(files.read_byte(fd).unwrap(), Some(255));
        files.unread_byte(fd, b'Q').unwrap();
        assert_eq!(files.tell(fd).unwrap(), 1);
        assert!(files.unread_byte(fd, b'R').is_err());
        assert_eq!(files.read_byte(fd).unwrap(), Some(b'Q'));
        assert_eq!(files.tell(fd).unwrap(), 2);
        assert_eq!(files.read_byte(fd).unwrap(), Some(b'Z'));
        assert!(!files.eof(fd).unwrap());
        assert_eq!(files.read_byte(fd).unwrap(), None);
        assert!(files.eof(fd).unwrap());
        files.unread_byte(fd, b'X').unwrap();
        assert!(!files.eof(fd).unwrap());
        files.seek(fd, 0, 1).unwrap();
        assert_eq!(files.read_byte(fd).unwrap(), Some(b'Z'));
        assert_eq!(files.read_byte(fd).unwrap(), None);
        files.rewind(fd).unwrap();
        assert!(!files.eof(fd).unwrap());
        assert_eq!(files.tell(fd).unwrap(), 0);
        assert_eq!(files.read_byte(fd).unwrap(), Some(0));
        files.close(fd).unwrap();
        assert!(files.read_byte(fd).is_err());
        assert!(files.eof(fd).is_err());
        assert!(files.unread_byte(fd, 0).is_err());
        assert!(files.tell(fd).is_err());
        assert_eq!(std::fs::read(path).unwrap(), [0, 255, b'Z']);
    }

    #[test]
    fn eof_remains_latched_when_another_descriptor_appends_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("growing");
        std::fs::write(&path, []).unwrap();
        let mut files = FileIo::default();
        let reader = files.open(&path, Some("r"));
        assert_eq!(files.read_byte(reader).unwrap(), None);
        let writer = files.open(&path, Some("a"));
        files.write(writer, b"new").unwrap();
        assert_eq!(files.read_byte(reader).unwrap(), None);
        assert!(files.eof(reader).unwrap());
        files.seek(reader, 0, 1).unwrap();
        assert!(!files.eof(reader).unwrap());
        assert_eq!(files.read_byte(reader).unwrap(), Some(b'n'));
    }

    #[test]
    fn append_modes_open_at_eof_without_latching_eof_until_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("append-position");
        std::fs::write(&path, b"ab").unwrap();
        let mut files = FileIo::default();
        for mode in ["a", "ab", "a+", "a+b", "ab+"] {
            let fd = files.open(&path, Some(mode));
            assert_ne!(fd, 0, "{mode}");
            assert_eq!(files.tell(fd).unwrap(), 2, "{mode}");
            if mode.contains('+') {
                assert!(!files.eof(fd).unwrap(), "{mode}");
                assert_eq!(files.read_byte(fd).unwrap(), None, "{mode}");
                assert!(files.eof(fd).unwrap(), "{mode}");
                files.rewind(fd).unwrap();
                assert!(!files.eof(fd).unwrap(), "{mode}");
                assert_eq!(files.read_byte(fd).unwrap(), Some(b'a'), "{mode}");
            }
            files.close(fd).unwrap();
        }
        assert_eq!(std::fs::read(path).unwrap(), b"ab");
    }

    #[test]
    fn position_errors_preserve_pushback_and_append_ignores_seek_for_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("append");
        std::fs::write(&path, b"ab").unwrap();
        let mut files = FileIo::default();
        let fd = files.open(&path, Some("a+b"));
        files.rewind(fd).unwrap();
        assert_eq!(files.read_byte(fd).unwrap(), Some(b'a'));
        files.unread_byte(fd, b'Q').unwrap();
        assert!(files.seek(fd, -1, 0).is_err());
        assert!(files.seek(fd, 0, 9).is_err());
        assert_eq!(files.read_byte(fd).unwrap(), Some(b'Q'));
        files.rewind(fd).unwrap();
        files.write(fd, b"c").unwrap();
        assert_eq!(files.tell(fd).unwrap(), 3);
        files.seek(fd, -1, 2).unwrap();
        assert_eq!(files.read_byte(fd).unwrap(), Some(b'c'));
        files.close(fd).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"abc");
    }

    #[test]
    fn flush_validates_fanout_and_keeps_streams_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let mut files = FileIo::default();
        let channel = files.open(&path, None);
        files.write(channel, b"a").unwrap();
        files.flush(Some(channel | 1)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"a");
        assert!(files.flush(Some(channel | (1 << 30))).is_err());
        files.write(channel, b"b").unwrap();
        files.flush(None).unwrap();
        files.close(channel).unwrap();
        assert!(files.flush(Some(channel)).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"ab");
        files.close(FD_FLAG | 1).unwrap();
        assert!(files.flush(Some(FD_FLAG | 1)).is_err());
        assert!(files.flush(Some(1)).is_err());
        // Flush-all skips closed handles.
        files.flush(None).unwrap();
    }

    #[test]
    fn reads_and_positions_reject_mcds_and_write_only_handles() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let mut files = FileIo::default();
        let mcd = files.open(&path, None);
        assert!(files.read_byte(mcd).is_err());
        assert!(files.unread_byte(mcd, 1).is_err());
        assert!(files.eof(mcd).is_err());
        assert!(files.tell(mcd).is_err());
        assert!(files.rewind(mcd).is_err());
        let fd = files.open(&path, Some("w"));
        assert!(files.read_byte(fd).is_err());
        assert!(files.unread_byte(fd, 1).is_err());
        assert!(files.eof(fd).is_err());
        assert_eq!(files.tell(fd).unwrap(), 0);
        assert!(files.read_byte(FD_FLAG | 1).is_err());
        files.close(FD_FLAG).unwrap();
        assert!(files.read_byte(FD_FLAG).is_err());
    }

    #[test]
    fn descriptors_modes_channels_and_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let mut files = FileIo::default();
        let m1 = files.open(&a, None);
        let m2 = files.open(&b, None);
        assert_eq!((m1, m2), (2, 4));
        assert!(files.write(m1 | m2 | 1, b"both\n").unwrap());
        files.close(m1 | m2).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"both\n");
        assert_eq!(std::fs::read(&b).unwrap(), b"both\n");
        assert!(files.write(m1, b"closed").is_err());
        assert_eq!(files.open(&b, None), m1);
        let fd = files.open(&a, Some("ab+"));
        assert_eq!(fd, 0x8000_0003);
        files.write(fd, b"appended").unwrap();
        files.close(fd).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"both\nappended");
        assert_eq!(files.open(dir.path().join("missing"), Some("r")), 0);
        assert_eq!(files.open(&a, Some("bad")), 0);
        assert!(files.write(0x8000_0000, b"stdin").is_err());
    }

    #[test]
    fn stdout_selection_survives_mcd_errors_but_respects_closed_stdout() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("valid-channel.txt");
        let mut files = FileIo::default();
        let channel = files.open(&path, None);
        assert_ne!(channel, 0);
        let (stdout, result) = files.write_with_stdout(channel | 1 | (1 << 30), b"attempted");
        assert!(stdout);
        assert!(result.is_err());
        files.close(channel).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"attempted");
        files.close(1).unwrap();
        let (stdout, result) = files.write_with_stdout(1 | channel, b"closed");
        assert!(!stdout);
        assert!(result.is_err());
        let (stdout, result) = files.write_with_stdout(FD_FLAG | 1, b"closed");
        assert!(!stdout);
        assert!(result.is_err());
    }

    #[test]
    fn standard_file_descriptors_cannot_be_closed_twice() {
        for descriptor in [FD_FLAG, FD_FLAG | 1, FD_FLAG | 2] {
            let mut files = FileIo::default();
            files.close(descriptor).unwrap();
            assert_eq!(
                files.close(descriptor).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(files.write(descriptor, b"").is_err());
        }
    }

    #[test]
    fn stdout_fd_and_mcd_share_close_state_without_skipping_other_channels() {
        for (first, repeated) in [(1, FD_FLAG | 1), (FD_FLAG | 1, 1)] {
            let mut files = FileIo::default();
            files.close(first).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("fanout.txt");
            let channel = files.open(&path, None);
            files.write(channel, b"kept").unwrap();
            assert!(files.close(repeated).is_err());
            // An invalid stdout component must not skip closing valid MCDs.
            assert!(files.close(channel | 1).is_err());
            assert!(files.write(channel, b"closed").is_err());
            assert_eq!(std::fs::read(path).unwrap(), b"kept");
            // Closing stdout must not affect the other standard stream.
            files.close(FD_FLAG | 2).unwrap();
        }
    }

    #[test]
    fn zero_mcd_is_invalid_even_for_an_empty_write() {
        let mut files = FileIo::default();
        assert_eq!(
            files.write(0, b"").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let (stdout, result) = files.write_with_stdout(0, b"lost");
        assert!(!stdout);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn zero_mcd_cannot_be_closed() {
        let mut files = FileIo::default();
        assert_eq!(
            files.close(0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        // An invalid close must not close stdout or an unrelated valid file.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("still-open.txt");
        let channel = files.open(&path, None);
        assert!(files.close(0).is_err());
        assert!(files.write(channel | 1, b"open").unwrap());
        files.close(channel).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"open");
    }

    #[test]
    fn memories_preserve_four_state_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory");
        let value = BigUint::from(0xa5u8);
        let mask = BigUint::from(0xf0u8);
        let word = || DisplayFormatArg {
            value: &value,
            mask: Some(&mask),
            width: 8,
            signed: false,
            is_string: false,
            float_precision: None,
        };
        FileIo::write_memory(&path, true, [word()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xzxz0101\n");
        FileIo::write_memory(&path, false, [word()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "X5\n");
    }
}
