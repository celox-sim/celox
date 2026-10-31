//! An embedding-host fixture, not a production filesystem. Its private JSON
//! snapshot format demonstrates that Celox does not interpret backend state.
use celox::{FileMode, FileSystem};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stream {
    path: String,
    position: u64,
    read: bool,
    write: bool,
    append: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestIo {
    files: BTreeMap<String, Vec<u8>>,
    streams: BTreeMap<u64, Stream>,
    next: u64,
    stdin: u64,
    stderr: Vec<u8>,
    pub reject_restore: bool,
    pub format: String,
}
impl Default for TestIo {
    fn default() -> Self {
        Self {
            files: BTreeMap::from([("input".into(), b"12Q34".to_vec())]),
            streams: BTreeMap::new(),
            next: 0,
            stdin: 0,
            stderr: Vec::new(),
            reject_restore: false,
            format: "celox-test-host/v1".into(),
        }
    }
}
impl TestIo {
    fn stream(&mut self, id: u64) -> io::Result<&mut Stream> {
        self.streams
            .get_mut(&id)
            .ok_or_else(|| io::Error::other("closed handle"))
    }
}
impl FileSystem for TestIo {
    fn open(&mut self, path: &Path, mode: FileMode) -> io::Result<u64> {
        let path = path.to_str().unwrap().to_owned();
        if mode.create {
            self.files.entry(path.clone()).or_default();
        }
        let bytes = self
            .files
            .get_mut(&path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if mode.truncate {
            bytes.clear();
        }
        let id = self.next;
        self.next += 1;
        self.streams.insert(
            id,
            Stream {
                path,
                position: 0,
                read: mode.read,
                write: mode.write,
                append: mode.append,
            },
        );
        Ok(id)
    }
    fn close(&mut self, id: u64) -> io::Result<()> {
        self.streams
            .remove(&id)
            .ok_or_else(|| io::Error::other("closed handle"))?;
        Ok(())
    }
    fn read(&mut self, id: u64, bytes: &mut [u8]) -> io::Result<usize> {
        let stream = self.stream(id)?.clone();
        if !stream.read {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut cursor = io::Cursor::new(&self.files[&stream.path]);
        cursor.set_position(stream.position);
        let count = cursor.read(bytes)?;
        self.stream(id)?.position += count as u64;
        Ok(count)
    }
    fn write(&mut self, id: u64, bytes: &[u8]) -> io::Result<usize> {
        let stream = self.stream(id)?.clone();
        if !stream.write {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let data = self.files.get_mut(&stream.path).unwrap();
        let position = if stream.append {
            data.len() as u64
        } else {
            stream.position
        };
        let mut cursor = io::Cursor::new(data);
        cursor.set_position(position);
        let count = cursor.write(bytes)?;
        self.stream(id)?.position = position + count as u64;
        Ok(count)
    }
    fn seek(&mut self, id: u64, position: SeekFrom) -> io::Result<u64> {
        let stream = self.stream(id)?.clone();
        let mut cursor = io::Cursor::new(&self.files[&stream.path]);
        cursor.set_position(stream.position);
        let result = cursor.seek(position)?;
        self.stream(id)?.position = result;
        Ok(result)
    }
    fn flush(&mut self, id: u64) -> io::Result<()> {
        self.stream(id)?;
        Ok(())
    }
    fn read_stdin(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut cursor = io::Cursor::new(b"stdin");
        cursor.set_position(self.stdin);
        let count = cursor.read(bytes)?;
        self.stdin += count as u64;
        Ok(count)
    }
    fn write_stderr(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn snapshot_format(&self) -> &str {
        &self.format
    }
    fn snapshot(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(io::Error::other)
    }
    fn restore(&self, state: &[u8]) -> io::Result<Box<dyn FileSystem>> {
        if self.reject_restore {
            return Err(io::Error::other("host rejected restore"));
        }
        let state: Self = serde_json::from_slice(state).map_err(io::Error::other)?;
        Ok(Box::new(state))
    }
}
