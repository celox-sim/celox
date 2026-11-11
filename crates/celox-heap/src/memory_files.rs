//! Memory files `$readmemh` and `$readmemb` load while the design runs
//! (IEEE 1800-2023 21.4). The host reads and parses the file when compiled
//! code calls [`crate::ops::MEMORY_FILE_LOAD`]; the code then stores the
//! words the file gives, one at a time.

use std::sync::Arc;

use crate::{EMPTY_STRING, Handle, Heap};

/// How the host loads a memory file for compiled code.
pub trait MemoryFileReader: Send + Sync {
    /// Run the load `descriptor` describes, an encoding of the call's
    /// constant parameters that the compiler chose, reading file `file`;
    /// `range` holds the addresses the call loads from and toward when it
    /// gives them.
    fn load(
        &self,
        descriptor: &[u8],
        file: &[u8],
        range: Option<MemoryFileRange>,
    ) -> MemoryFileWords;
}

/// The addresses a memory file load is given at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryFileRange {
    pub start: i64,
    pub finish: i64,
    /// Whether either address has an unknown bit, which loads nothing.
    pub unknown: bool,
}

/// The words a memory file load gives, and its messages.
#[derive(Clone, Debug, Default)]
pub struct MemoryFileWords {
    /// The position of each word in the storage order of the array, or the
    /// index of the element of an associative array it stores.
    pub positions: Vec<u64>,
    /// The 64-bit chunks a word's plane takes.
    pub chunks: usize,
    /// The value plane of each word, `chunks` chunks each, least
    /// significant first.
    pub values: Vec<u64>,
    /// The unknown plane of each word, as `values`.
    pub masks: Vec<u64>,
    /// The warning of the load, such as a word count that differs from the
    /// range.
    pub warning: Option<Vec<u8>>,
    /// The error that terminated the load; the words before it are given.
    pub error: Option<Vec<u8>>,
}

/// The reader of a heap and the load compiled code is storing.
#[derive(Clone, Default)]
pub(crate) struct MemoryFiles {
    reader: Option<Arc<dyn MemoryFileReader>>,
    range: Option<MemoryFileRange>,
    loaded: MemoryFileWords,
}

impl std::fmt::Debug for MemoryFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryFiles")
            .field("reader", &self.reader.is_some())
            .field("range", &self.range)
            .field("loaded", &self.loaded)
            .finish()
    }
}

impl Heap {
    /// Load the memory files of compiled code through `reader`.
    pub fn set_memory_file_reader(&mut self, reader: Arc<dyn MemoryFileReader>) {
        self.memory_files.reader = Some(reader);
    }

    /// The reader [`Self::set_memory_file_reader`] installed.
    pub fn memory_file_reader(&self) -> Option<Arc<dyn MemoryFileReader>> {
        self.memory_files.reader.clone()
    }

    pub(crate) fn memory_file_call(
        &mut self,
        op: u64,
        parameter: u64,
        a: u64,
        b: u64,
        c: u64,
    ) -> u64 {
        let files = &mut self.memory_files;
        match op {
            crate::ops::MEMORY_FILE_RANGE => {
                files.range = Some(MemoryFileRange {
                    start: a as i64,
                    finish: b as i64,
                    unknown: c != 0,
                });
                0
            }
            crate::ops::MEMORY_FILE_LOAD => {
                let range = files.range.take();
                let descriptor = self.string(a as Handle).to_vec();
                let file = self.string(b as Handle).to_vec();
                let loaded = match &self.memory_files.reader {
                    Some(reader) => reader.load(&descriptor, &file, range),
                    None => MemoryFileWords {
                        error: Some(b"memory files cannot be read by this simulator".to_vec()),
                        ..MemoryFileWords::default()
                    },
                };
                let count = loaded.positions.len() as u64;
                self.memory_files.loaded = loaded;
                count
            }
            crate::ops::MEMORY_FILE_POSITION => {
                files.loaded.positions.get(a as usize).copied().unwrap_or(0)
            }
            crate::ops::MEMORY_FILE_WORD => {
                let plane = if parameter & 1 == 0 {
                    &files.loaded.values
                } else {
                    &files.loaded.masks
                };
                (a as usize)
                    .checked_mul(files.loaded.chunks)
                    .and_then(|index| index.checked_add(b as usize))
                    .and_then(|index| plane.get(index))
                    .copied()
                    .unwrap_or(0)
            }
            crate::ops::MEMORY_FILE_MESSAGE => {
                let message = if parameter & 1 == 0 {
                    files.loaded.warning.take()
                } else {
                    files.loaded.error.take()
                };
                match message {
                    Some(message) => self.intern(&message).into(),
                    None => EMPTY_STRING.into(),
                }
            }
            _ => 0,
        }
    }
}
