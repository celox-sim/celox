//! CLI file effects: a disk-backed working view plus immediate host output.
//! Checkpoints retain the working view; already delivered host output is not
//! undone. A later write publishes the new working contents again.
use celox::{FileMode, FileSystem};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stream {
    path: PathBuf,
    position: u64,
    read: bool,
    write: bool,
    append: bool,
    live: bool,
}
#[derive(Serialize, Deserialize)]
struct State {
    files: BTreeMap<PathBuf, Vec<u8>>,
    opaque_prefixes: BTreeMap<PathBuf, u64>,
    streams: BTreeMap<u64, Stream>,
    next: u64,
}
#[derive(Debug, Default)]
pub(super) struct CheckpointFs {
    files: BTreeMap<PathBuf, tempfile::TempPath>,
    opaque_prefixes: BTreeMap<PathBuf, u64>,
    live_files: BTreeMap<u64, File>,
    outputs: BTreeMap<u64, File>,
    pending_publish: BTreeSet<PathBuf>,
    streams: BTreeMap<u64, Stream>,
    next: u64,
}
// TempPath owns cleanup but no OS descriptor. Reopen only for each operation,
// including snapshot/restore, so closed files do not consume the descriptor limit.
fn working_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

impl CheckpointFs {
    fn stream(&self, id: u64) -> io::Result<Stream> {
        self.streams
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::other("closed file effect handle"))
    }
    fn live_file(&mut self, id: u64, stream: &Stream) -> io::Result<&mut File> {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.live_files.entry(id) {
            // External channels reconnect lazily: restoring a system checkpoint
            // must not block waiting for a pipe peer.
            entry.insert(
                OpenOptions::new()
                    .read(stream.read)
                    .write(stream.write)
                    .append(stream.append)
                    .open(&stream.path)?,
            );
        }
        Ok(self.live_files.get_mut(&id).unwrap())
    }
    fn output_file(&mut self, id: u64, stream: &Stream) -> io::Result<&mut File> {
        if let std::collections::btree_map::Entry::Vacant(entry) = self.outputs.entry(id) {
            // Restored streams reconnect to their recorded path on first output.
            // Ordinary active streams retain their original host handle until close.
            entry.insert(
                OpenOptions::new()
                    .write(true)
                    .append(stream.append)
                    .create(
                        self.pending_publish.contains(&stream.path)
                            && !self.opaque_prefixes.contains_key(&stream.path),
                    )
                    .open(&stream.path)?,
            );
        }
        Ok(self.outputs.get_mut(&id).unwrap())
    }
    fn publish(&mut self, id: u64, stream: &Stream) -> io::Result<()> {
        let key = &stream.path;
        let prefix = self.opaque_prefixes.get(key).copied().unwrap_or(0);
        let mut file = working_file(&self.files[key])?;
        file.seek(SeekFrom::Start(prefix))?;
        // Do not replace an unreadable, externally owned append prefix.
        let mut output = self.output_file(id, stream)?;
        if output.metadata()?.len() < prefix {
            return Err(io::Error::other(
                "external append prefix is shorter than the checkpoint",
            ));
        }
        output.set_len(prefix)?;
        output.seek(SeekFrom::Start(prefix))?;
        io::copy(&mut file, &mut output)?;
        output.flush()?;
        self.pending_publish.remove(key);
        Ok(())
    }
}
impl FileSystem for CheckpointFs {
    fn open(&mut self, path: &Path, mode: FileMode) -> io::Result<u64> {
        let absolute = std::path::absolute(path)?;
        let path = std::fs::canonicalize(&absolute).unwrap_or(absolute);
        let live = std::fs::metadata(&path).is_ok_and(|metadata| !metadata.is_file());
        if live {
            let file = OpenOptions::new()
                .read(mode.read)
                .write(mode.write)
                .append(mode.append)
                .create(mode.create)
                .truncate(mode.truncate)
                .open(&path)?;
            let id = self.next;
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| io::Error::other("file handle space exhausted"))?;
            self.live_files.insert(id, file);
            self.streams.insert(
                id,
                Stream {
                    path,
                    position: 0,
                    read: mode.read,
                    write: mode.write,
                    append: mode.append,
                    live: true,
                },
            );
            return Ok(id);
        }
        // Request only the permissions in the HDL mode. In particular, append
        // is a write effect and does not grant read access to an existing prefix.
        let cached = self.files.contains_key(&path);
        let needs_host =
            mode.write || !cached || (mode.read && self.opaque_prefixes.contains_key(&path));
        let host = if needs_host {
            Some(
                OpenOptions::new()
                    .read(mode.read)
                    .write(mode.write)
                    .append(mode.append)
                    .create(mode.create)
                    .truncate(mode.truncate)
                    .open(&path)?,
            )
        } else {
            None
        };
        let key = path.clone();
        if !self.files.contains_key(&key) {
            let mut file = tempfile::NamedTempFile::new()?;
            if !mode.truncate {
                match File::open(&path) {
                    Ok(mut source) => {
                        io::copy(&mut source, &mut file)?;
                    }
                    Err(error)
                        if mode.append
                            && !mode.read
                            && error.kind() == io::ErrorKind::PermissionDenied =>
                    {
                        let prefix = host
                            .as_ref()
                            .expect("append file opened above")
                            .metadata()?
                            .len();
                        file.as_file().set_len(prefix)?;
                        self.opaque_prefixes.insert(key.clone(), prefix);
                    }
                    Err(error) => return Err(error),
                }
            }
            self.files.insert(key.clone(), file.into_temp_path());
        }
        if mode.truncate {
            working_file(&self.files[&key])?.set_len(0)?;
            self.opaque_prefixes.remove(&key);
            self.pending_publish.remove(&key);
        }
        if mode.read
            && let Some(prefix) = self.opaque_prefixes.get(&key).copied()
        {
            let mut source = File::open(&path)?.take(prefix);
            let mut file = working_file(&self.files[&key])?;
            file.rewind()?;
            if io::copy(&mut source, &mut file)? != prefix {
                return Err(io::Error::other("external append prefix is truncated"));
            }
            self.opaque_prefixes.remove(&key);
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("file handle space exhausted"))?;
        if mode.write {
            self.outputs.insert(id, host.expect("write opened above"));
        }
        self.streams.insert(
            id,
            Stream {
                path,
                position: 0,
                read: mode.read,
                write: mode.write,
                append: mode.append,
                live: false,
            },
        );
        Ok(id)
    }
    fn close(&mut self, id: u64) -> io::Result<()> {
        self.live_files.remove(&id);
        self.outputs.remove(&id);
        self.streams
            .remove(&id)
            .ok_or_else(|| io::Error::other("closed file effect handle"))?;
        Ok(())
    }
    fn read(&mut self, id: u64, bytes: &mut [u8]) -> io::Result<usize> {
        let stream = self.stream(id)?;
        if stream.live {
            return self.live_file(id, &stream)?.read(bytes);
        }
        if !stream.read {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut file = working_file(&self.files[&stream.path])?;
        file.seek(SeekFrom::Start(stream.position))?;
        let count = file.read(bytes)?;
        self.streams.get_mut(&id).unwrap().position += count as u64;
        Ok(count)
    }
    fn write(&mut self, id: u64, bytes: &[u8]) -> io::Result<usize> {
        let stream = self.stream(id)?;
        if stream.live {
            return self.live_file(id, &stream)?.write(bytes);
        }
        if !stream.write {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if self.pending_publish.contains(&stream.path) {
            self.publish(id, &stream)?;
        }
        let mut file = working_file(&self.files[&stream.path])?;
        let position = file.seek(if stream.append {
            SeekFrom::End(0)
        } else {
            SeekFrom::Start(stream.position)
        })?;
        let count = file.write(bytes)?;
        self.streams.get_mut(&id).unwrap().position = position + count as u64;
        // Ordinary writes publish only their byte range. A restored branch
        // materializes its saved baseline once, before its first output effect.
        let output = self.output_file(id, &stream)?;
        if !stream.append {
            output.seek(SeekFrom::Start(position))?;
        }
        output.write_all(&bytes[..count])?;
        Ok(count)
    }
    fn seek(&mut self, id: u64, from: SeekFrom) -> io::Result<u64> {
        let stream = self.stream(id)?;
        if stream.live {
            return self.live_file(id, &stream)?.seek(from);
        }
        let mut file = working_file(&self.files[&stream.path])?;
        file.seek(SeekFrom::Start(stream.position))?;
        let position = file.seek(from)?;
        self.streams.get_mut(&id).unwrap().position = position;
        Ok(position)
    }
    fn flush(&mut self, id: u64) -> io::Result<()> {
        let stream = self.stream(id)?;
        if stream.live {
            self.live_file(id, &stream)?.flush()?;
        }
        Ok(())
    }
    // Standard streams are live external channels, not files in the working view.
    fn read_stdin(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        io::stdin().lock().read(bytes)
    }
    fn write_stderr(&mut self, bytes: &[u8]) -> io::Result<()> {
        io::stderr().lock().write_all(bytes)
    }
    fn flush_stdout(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()
    }
    fn flush_stderr(&mut self) -> io::Result<()> {
        io::stderr().lock().flush()
    }
    fn snapshot_format(&self) -> &str {
        "celox-cli-file-effects/v3"
    }
    fn snapshot(&self) -> io::Result<Vec<u8>> {
        let mut files = BTreeMap::new();
        for (path, file) in &self.files {
            let mut file = working_file(file)?;
            file.seek(SeekFrom::Start(
                self.opaque_prefixes.get(path).copied().unwrap_or(0),
            ))?;
            let mut bytes = Vec::new();
            let read = file.read_to_end(&mut bytes);
            read?;
            files.insert(path.clone(), bytes);
        }
        serde_json::to_vec(&State {
            files,
            streams: self.streams.clone(),
            opaque_prefixes: self.opaque_prefixes.clone(),
            next: self.next,
        })
        .map_err(io::Error::other)
    }
    fn restore(&self, bytes: &[u8]) -> io::Result<Box<dyn FileSystem>> {
        let state: State = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let mut files = BTreeMap::new();
        for (path, bytes) in state.files {
            let mut file = tempfile::NamedTempFile::new()?;
            let prefix = state.opaque_prefixes.get(&path).copied().unwrap_or(0);
            file.as_file().set_len(prefix)?;
            file.seek(SeekFrom::Start(prefix))?;
            file.write_all(&bytes)?;
            files.insert(path, file.into_temp_path());
        }
        if state.streams.iter().any(|(id, stream)| {
            *id >= state.next || (!stream.live && !files.contains_key(&stream.path))
        }) {
            return Err(io::Error::other("invalid file effect state"));
        }
        Ok(Box::new(Self {
            pending_publish: files.keys().cloned().collect(),
            files,
            opaque_prefixes: state.opaque_prefixes,
            live_files: BTreeMap::new(),
            outputs: BTreeMap::new(),
            streams: state.streams,
            next: state.next,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_keeps_file_effect_state_separate_from_delivered_output() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let mode = FileMode {
            read: true,
            write: true,
            create: true,
            truncate: true,
            append: false,
        };
        let mut effects = CheckpointFs::default();
        let fd = effects.open(&path, mode).unwrap();
        effects.write(fd, b"before").unwrap();
        effects.seek(fd, SeekFrom::Start(0)).unwrap();
        let saved = effects.snapshot().unwrap();
        effects.write(fd, b"after!").unwrap();
        effects.close(fd).unwrap();
        let mut restored = effects.restore(&saved).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"after!");
        let mut buffer = [0; 6];
        restored.read(fd, &mut buffer).unwrap();
        assert_eq!(&buffer, b"before");
        restored.write(fd, b" again").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"before again");
        let mut second = effects.restore(&saved).unwrap();
        second.read(fd, &mut buffer).unwrap();
        assert_eq!(&buffer, b"before");
    }
}

#[cfg(all(test, unix))]
#[test]
fn device_effects_reconnect_as_live_channels() {
    let mode = FileMode {
        read: true,
        write: true,
        append: false,
        create: false,
        truncate: false,
    };
    let mut effects = CheckpointFs::default();
    let fd = effects.open(Path::new("/dev/null"), mode).unwrap();
    let saved = effects.snapshot().unwrap();
    effects.close(fd).unwrap();
    let mut restored = effects.restore(&saved).unwrap();
    assert_eq!(restored.write(fd, b"output").unwrap(), 6);
    assert_eq!(restored.read(fd, &mut [0; 1]).unwrap(), 0);
}

#[cfg(all(test, unix))]
#[test]
fn write_only_append_preserves_the_external_prefix_across_restore() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("write-only");
    std::fs::write(&path, b"prefix").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o200)).unwrap();
    // Root can read despite the permission bits, but still exercises append.
    let unreadable = File::open(&path).is_err();
    let mode = FileMode {
        read: false,
        write: true,
        append: true,
        create: true,
        truncate: false,
    };
    let mut effects = CheckpointFs::default();
    let fd = effects.open(&path, mode).unwrap();
    if unreadable {
        assert_eq!(
            effects
                .opaque_prefixes
                .values()
                .copied()
                .collect::<Vec<_>>(),
            [6]
        );
    }
    effects.write(fd, b"A").unwrap();
    let saved = effects.snapshot().unwrap();
    effects.write(fd, b"B").unwrap();
    let mut restored = effects.restore(&saved).unwrap();
    restored.write(fd, b"C").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"prefixAC");
}

#[cfg(all(test, unix))]
#[test]
fn closed_working_files_do_not_retain_descriptors() {
    const CHILD: &str = "CELOX_TEST_BOUNDED_FILE_DESCRIPTORS";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "checkpoint_fs::closed_working_files_do_not_retain_descriptors",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    // This subprocess contains only this test; do not change the parent runner's limit.
    let limit = libc::rlimit {
        rlim_cur: 64,
        rlim_max: 64,
    };
    // SAFETY: pointer to an initialized rlimit; no pointers retained by the OS.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let directory = tempfile::tempdir().unwrap();
    let mode = FileMode {
        read: true,
        write: true,
        create: true,
        truncate: false,
        append: false,
    };
    let mut effects = CheckpointFs::default();
    for index in 0..128 {
        let fd = effects
            .open(&directory.path().join(index.to_string()), mode)
            .unwrap();
        effects.write(fd, &[index]).unwrap();
        effects.close(fd).unwrap();
    }
    let saved = effects.snapshot().unwrap();
    let mut restored = effects.restore(&saved).unwrap();
    for index in 0..128 {
        let path = directory.path().join(index.to_string());
        std::fs::write(&path, b"external").unwrap();
        let fd = restored.open(&path, mode).unwrap();
        let mut byte = [0];
        assert_eq!(restored.read(fd, &mut byte).unwrap(), 1);
        assert_eq!(byte, [index]);
        restored.close(fd).unwrap();
    }
}

#[cfg(test)]
#[test]
fn closed_working_contents_can_be_reopened_without_the_host_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input");
    std::fs::write(&path, b"saved").unwrap();
    let mode = FileMode {
        read: true,
        write: false,
        append: false,
        create: false,
        truncate: false,
    };
    let mut effects = CheckpointFs::default();
    let fd = effects.open(&path, mode).unwrap();
    effects.close(fd).unwrap();
    let saved = effects.snapshot().unwrap();
    std::fs::remove_file(&path).unwrap();
    let mut restored = effects.restore(&saved).unwrap();
    let fd = restored.open(&path, mode).unwrap();
    let mut bytes = [0; 5];
    assert_eq!(restored.read(fd, &mut bytes).unwrap(), 5);
    assert_eq!(&bytes, b"saved");
}

#[cfg(all(test, unix))]
#[test]
fn active_outputs_keep_the_open_file_across_rotation_and_unlink() {
    for append in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output");
        let rotated = directory.path().join("rotated");
        let mut effects = CheckpointFs::default();
        let mode = FileMode {
            read: true,
            write: true,
            create: true,
            truncate: false,
            append,
        };
        let fd = effects.open(&path, mode).unwrap();
        effects.write(fd, b"A").unwrap();
        std::fs::rename(&path, &rotated).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        effects.write(fd, b"B").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        assert_eq!(std::fs::read(&rotated).unwrap(), b"AB");
        let mut original = File::open(&rotated).unwrap();
        std::fs::remove_file(&rotated).unwrap();
        std::fs::remove_file(&path).unwrap();
        effects.write(fd, b"C").unwrap();
        effects.close(fd).unwrap();
        let mut bytes = Vec::new();
        original.read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ABC");
        assert!(!path.exists());
    }
}
