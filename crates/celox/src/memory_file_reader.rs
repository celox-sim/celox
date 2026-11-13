//! The memory-file reader of the heap: compiled code calls it when a
//! `$readmemh` or `$readmemb` executes (IEEE 1800-2023 21.4).

use std::path::Path;
use std::sync::Arc;

use celox_design::MemoryFileBase;
use celox_frontend_core::memory_file::{MemoryLoad, decode_descriptor, load_memory_file};
use celox_heap::memory_files::{MemoryFileRange, MemoryFileReader, MemoryFileWords};
use num_bigint::BigUint;

type ReadFile = dyn Fn(&Path) -> std::io::Result<String> + Send + Sync;

/// Reads the memory files compiled code loads, through a file system: the
/// host's, or a simulation's.
pub struct HostMemoryFileReader {
    read: Box<ReadFile>,
    /// The directories relative names are looked up in, by key.
    bases: crate::HashMap<u64, String>,
}

impl HostMemoryFileReader {
    /// A reader of the host file system for a design whose loads look
    /// relative names up in `bases` (see `RuntimeDesign`'s memory file bases).
    pub fn new(bases: &[MemoryFileBase]) -> Self {
        Self::with_reader(bases, |path| std::fs::read_to_string(path))
    }

    pub(crate) fn with_reader(
        bases: &[MemoryFileBase],
        read: impl Fn(&Path) -> std::io::Result<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            read: Box::new(read),
            bases: bases
                .iter()
                .map(|base| (base.key, base.path.clone()))
                .collect(),
        }
    }

    /// The reader, for a heap.
    pub fn into_heap_reader(self) -> Arc<dyn MemoryFileReader> {
        Arc::new(self)
    }
}

impl MemoryFileReader for HostMemoryFileReader {
    fn load(
        &self,
        descriptor: &[u8],
        file: &[u8],
        range: Option<MemoryFileRange>,
    ) -> MemoryFileWords {
        let addresses = range.map(|range| (range.start, range.finish));
        let Some(mut load) = decode_descriptor(descriptor, file, addresses) else {
            return MemoryFileWords {
                error: Some(b"invalid memory file load".to_vec()),
                ..Default::default()
            };
        };
        // An address with an unknown bit names no word (IEEE 1800-2023 21.4).
        if range.is_some_and(|range| range.unknown) {
            return MemoryFileWords {
                error: Some(format!("{}: address contains X or Z", load.task()).into_bytes()),
                ..Default::default()
            };
        }
        if let Some(key) = load.base_key {
            load.base = self.bases.get(&key).cloned();
        }
        let outcome = load_memory_file(&load, |path| (self.read)(path));
        memory_file_words(load.element_width, outcome)
    }
}

/// The words of `outcome`, whose words are `width` bits wide, one by one.
fn memory_file_words(width: usize, outcome: MemoryLoad) -> MemoryFileWords {
    let chunks = width.div_ceil(64);
    let mut words = MemoryFileWords {
        chunks,
        warning: outcome.warning.map(String::into_bytes),
        error: outcome.error.map(String::into_bytes),
        ..Default::default()
    };
    if width == 0 {
        return words;
    }
    // A word of an associative array is placed by its index.
    for word in outcome.keyed {
        words.positions.push(word.address);
        for (plane, out) in [
            (&word.value, &mut words.values),
            (&word.mask, &mut words.masks),
        ] {
            let mut digits = plane.to_u64_digits();
            digits.resize(chunks, 0);
            out.extend(digits);
        }
    }
    let word_mask = (BigUint::from(1u8) << width) - 1u8;
    for run in outcome.runs {
        let value = BigUint::from_bytes_le(&run.value_bytes);
        let mask = BigUint::from_bytes_le(&run.mask_bytes);
        for k in 0..run.bit_width / width {
            words.positions.push((run.bit_offset / width + k) as u64);
            for (plane, out) in [(&value, &mut words.values), (&mask, &mut words.masks)] {
                let mut digits = ((plane >> (k * width)) & &word_mask).to_u64_digits();
                digits.resize(chunks, 0);
                out.extend(digits);
            }
        }
    }
    words
}
