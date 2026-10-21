//! SystemVerilog file descriptors and multichannel descriptors (IEEE 1800-2023 21.3).
//!
//! A manager belongs to one simulation. No process-global descriptor table is used.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use celox_testbench::{DisplayFormatArg, FieldSpec, format_sv_display_arg};

const FD_FLAG: u32 = 0x8000_0000;

#[derive(Debug)]
pub struct FileIo {
    channels: BTreeMap<u32, File>,
    descriptors: BTreeMap<u32, File>,
    stdin_open: bool,
    stdout_open: bool,
    stderr_open: bool,
}

impl Default for FileIo {
    fn default() -> Self {
        Self {
            channels: BTreeMap::new(),
            descriptors: BTreeMap::new(),
            stdin_open: true,
            stdout_open: true,
            stderr_open: true,
        }
    }
}

impl FileIo {
    /// Open an MCD when mode is absent, otherwise an FD. Failure returns zero,
    /// as specified by 21.3.1. Closed channel numbers are reused.
    pub fn open(&mut self, path: impl AsRef<Path>, mode: Option<&str>) -> u32 {
        let mut options = OpenOptions::new();
        match mode.unwrap_or("w") {
            "r" | "rb" => {
                options.read(true);
            }
            "w" | "wb" => {
                options.write(true).create(true).truncate(true);
            }
            "a" | "ab" => {
                options.append(true).create(true);
            }
            "r+" | "r+b" | "rb+" => {
                options.read(true).write(true);
            }
            "w+" | "w+b" | "wb+" => {
                options.read(true).write(true).create(true).truncate(true);
            }
            "a+" | "a+b" | "ab+" => {
                options.read(true).append(true).create(true);
            }
            _ => return 0,
        }
        let (table, key) = if mode.is_some() {
            let Some(key) = (3..FD_FLAG).find(|key| !self.descriptors.contains_key(key)) else {
                return 0;
            };
            (&mut self.descriptors, key)
        } else {
            let Some(key) = (1..31)
                .map(|bit| 1u32 << bit)
                .find(|key| !self.channels.contains_key(key))
            else {
                return 0;
            };
            (&mut self.channels, key)
        };
        let Ok(file) = options.open(path) else {
            return 0;
        };
        table.insert(key, file);
        if mode.is_some() { key | FD_FLAG } else { key }
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
            let Some(mut file) = self.descriptors.remove(&key) else {
                return Err(invalid_descriptor());
            };
            return file.flush();
        }
        let mut error = None;
        if descriptor & 1 != 0 && !std::mem::replace(&mut self.stdout_open, false) {
            error = Some(invalid_descriptor());
        }
        for bit in 1..31 {
            let key = 1u32 << bit;
            if descriptor & key != 0 {
                match self.channels.remove(&key) {
                    Some(mut file) => {
                        if let Err(err) = file.flush() {
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
                    io::stderr().lock().write_all(bytes)?;
                    Ok(false)
                }
                key => {
                    self.descriptors
                        .get_mut(&key)
                        .ok_or_else(invalid_descriptor)?
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

    /// Write a static unpacked memory snapshot, without address specifiers
    /// (21.5.3). The caller supplies elements in the requested address order.
    pub fn write_memory<'a>(
        path: impl AsRef<Path>,
        binary: bool,
        words: impl IntoIterator<Item = DisplayFormatArg<'a>>,
    ) -> io::Result<()> {
        let mut file = File::create(path)?;
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
        };
        FileIo::write_memory(&path, true, [word()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "xzxz0101\n");
        FileIo::write_memory(&path, false, [word()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "X5\n");
    }
}
