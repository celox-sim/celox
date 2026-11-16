//! Sets of logical values.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in super::super) struct LogicalValue(pub u32);

/// Mutable W/S frontiers stay sorted; completed large S rows use indexed
/// 8/16-bit offsets or a dense bitmap, whichever is smaller. These preserve exact
/// ascending iteration and membership without a full value per CFG row.
#[derive(Debug, Clone)]
pub(in super::super) enum LogicalSet {
    Mutable(Vec<LogicalValue>),
    Frozen(std::sync::Arc<FrozenLogicalSet>),
}

#[derive(Debug)]
pub(in super::super) struct FrozenLogicalSet {
    pub(super) values: std::sync::Arc<[LogicalValue]>,
    pub(super) storage: FrozenLogicalStorage,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FrozenLogicalStorage {
    Sparse8 {
        // Each chunk contains at most 64 values within a u8 span.
        chunks: Box<[(u32, u32)]>,
        offsets: Box<[u8]>,
    },
    Sparse {
        // Each chunk contains at most 64 values within a u16 span.
        chunks: Box<[(u32, u32)]>, // (base value, first offset index)
        offsets: Box<[u16]>,
    },
    Dense {
        words: Box<[u64]>,
        len: usize,
    },
}

impl Default for LogicalSet {
    fn default() -> Self {
        Self::Mutable(Vec::new())
    }
}

impl PartialEq for LogicalSet {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Mutable(left), Self::Mutable(right)) => left == right,
            (Self::Frozen(left), Self::Frozen(right)) if left.storage == right.storage => true,
            _ => self.len() == other.len() && self.iter().eq(other.iter()),
        }
    }
}

impl Eq for LogicalSet {}

impl LogicalSet {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn freeze(&mut self, pool: &std::sync::Arc<[LogicalValue]>) {
        let Self::Mutable(values) = self else {
            return;
        };
        if values.len() < 64
            || values
                .last()
                .is_some_and(|value| value.0 as usize >= pool.len())
        {
            return;
        }
        let mut chunks = Vec::new();
        let mut offsets = Vec::with_capacity(values.len());
        let mut base = 0;
        let mut chunk_start = 0;
        let mut byte_chunks = Vec::new();
        let mut byte_base = 0;
        let mut byte_chunk_start = 0;
        for (index, value) in values.iter().enumerate() {
            if index == 0 || index - chunk_start == 64 || value.0 - base > u32::from(u16::MAX) {
                let Ok(offset) = u32::try_from(index) else {
                    return;
                };
                base = value.0;
                chunk_start = index;
                chunks.push((base, offset));
            }
            offsets.push((value.0 - base) as u16);
            if index == 0
                || index - byte_chunk_start == 64
                || value.0 - byte_base > u32::from(u8::MAX)
            {
                let Ok(offset) = u32::try_from(index) else {
                    return;
                };
                byte_base = value.0;
                byte_chunk_start = index;
                byte_chunks.push((byte_base, offset));
            }
        }
        let sparse_bytes = offsets.len() * 2 + chunks.len() * 8;
        let byte_sparse_bytes = values.len() + byte_chunks.len() * 8;
        let word_count = values.last().unwrap().0 as usize / 64 + 1;
        let dense_bytes = word_count * std::mem::size_of::<u64>();
        let storage = if dense_bytes < sparse_bytes.min(byte_sparse_bytes)
            && dense_bytes < values.len() * 4
        {
            let mut words = vec![0u64; word_count];
            for value in values.iter() {
                words[value.0 as usize / 64] |= 1u64 << (value.0 % 64);
            }
            FrozenLogicalStorage::Dense {
                words: words.into_boxed_slice(),
                len: values.len(),
            }
        } else if byte_sparse_bytes < sparse_bytes && byte_sparse_bytes < values.len() * 4 {
            let mut chunk = 0;
            let offsets = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    if byte_chunks
                        .get(chunk + 1)
                        .is_some_and(|&(_, start)| index == start as usize)
                    {
                        chunk += 1;
                    }
                    (value.0 - byte_chunks[chunk].0) as u8
                })
                .collect();
            FrozenLogicalStorage::Sparse8 {
                chunks: byte_chunks.into_boxed_slice(),
                offsets,
            }
        } else if sparse_bytes < values.len() * 4 {
            FrozenLogicalStorage::Sparse {
                chunks: chunks.into_boxed_slice(),
                offsets: offsets.into_boxed_slice(),
            }
        } else {
            return;
        };
        *self = Self::Frozen(std::sync::Arc::new(FrozenLogicalSet {
            values: std::sync::Arc::clone(pool),
            storage,
        }));
    }

    pub(super) fn mutable(&mut self) -> &mut Vec<LogicalValue> {
        if matches!(self, Self::Frozen(_)) {
            *self = Self::Mutable(self.iter().copied().collect());
        }
        let Self::Mutable(values) = self else {
            unreachable!()
        };
        values
    }

    #[cfg(test)]
    pub(in super::super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(in super::super) fn contains(&self, value: &LogicalValue) -> bool {
        match self {
            Self::Mutable(values) => values.binary_search(value).is_ok(),
            Self::Frozen(row) => match &row.storage {
                FrozenLogicalStorage::Sparse8 { chunks, offsets } => {
                    sparse_contains(chunks, offsets, value.0)
                }
                FrozenLogicalStorage::Dense { words, .. } => words
                    .get(value.0 as usize / 64)
                    .is_some_and(|word| word & (1u64 << (value.0 % 64)) != 0),
                FrozenLogicalStorage::Sparse { chunks, offsets } => {
                    sparse_contains(chunks, offsets, value.0)
                }
            },
        }
    }

    pub(in super::super) fn insert(&mut self, value: LogicalValue) -> bool {
        let values = self.mutable();
        let Err(index) = values.binary_search(&value) else {
            return false;
        };
        values.insert(index, value);
        true
    }

    pub(super) fn remove(&mut self, value: &LogicalValue) -> bool {
        let values = self.mutable();
        let Ok(index) = values.binary_search(value) else {
            return false;
        };
        values.remove(index);
        true
    }

    pub(super) fn len(&self) -> usize {
        match self {
            Self::Mutable(values) => values.len(),
            Self::Frozen(row) => match &row.storage {
                FrozenLogicalStorage::Sparse8 { offsets, .. } => offsets.len(),
                FrozenLogicalStorage::Sparse { offsets, .. } => offsets.len(),
                FrozenLogicalStorage::Dense { len, .. } => *len,
            },
        }
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(super) fn iter(&self) -> LogicalIter<'_> {
        LogicalIter {
            set: self,
            position: 0,
            chunk: 0,
            remaining_word: 0,
        }
    }
    pub(super) fn retain(&mut self, keep: impl FnMut(&LogicalValue) -> bool) {
        self.mutable().retain(keep);
    }
    pub(super) fn difference<'a>(
        &'a self,
        other: &'a Self,
    ) -> impl Iterator<Item = &'a LogicalValue> + 'a {
        let mut membership = OrderedMembership::new(other, self.len());
        self.iter().filter(move |value| !membership.contains(value))
    }
}

/// Membership queries in ascending value order can share one merge cursor.
/// Keep direct lookup for bitmaps and for a few queries into a much larger set.
pub(super) enum OrderedMembership<'a> {
    Lookup(&'a LogicalSet),
    Merge(std::iter::Peekable<LogicalIter<'a>>),
}

impl<'a> OrderedMembership<'a> {
    pub(super) fn new(set: &'a LogicalSet, queries: usize) -> Self {
        if set.len() > queries.saturating_mul(8)
            || matches!(set, LogicalSet::Frozen(row) if matches!(&row.storage, FrozenLogicalStorage::Dense { .. }))
        {
            Self::Lookup(set)
        } else {
            Self::Merge(set.iter().peekable())
        }
    }

    pub(super) fn contains(&mut self, value: &LogicalValue) -> bool {
        match self {
            Self::Lookup(set) => set.contains(value),
            Self::Merge(values) => {
                while values.peek().is_some_and(|&&next| next < *value) {
                    values.next();
                }
                values.peek().is_some_and(|&&next| next == *value)
            }
        }
    }
}

pub(super) fn sparse_contains<T: Ord + TryFrom<u32>>(
    chunks: &[(u32, u32)],
    offsets: &[T],
    value: u32,
) -> bool {
    let Some(chunk) = chunks
        .partition_point(|&(base, _)| base <= value)
        .checked_sub(1)
    else {
        return false;
    };
    let (base, start) = chunks[chunk];
    let Ok(offset) = T::try_from(value - base) else {
        return false;
    };
    let end = chunks
        .get(chunk + 1)
        .map_or(offsets.len(), |&(_, end)| end as usize);
    offsets[start as usize..end].binary_search(&offset).is_ok()
}

pub(in super::super) struct LogicalIter<'a> {
    pub(super) set: &'a LogicalSet,
    pub(super) position: usize,
    pub(super) chunk: usize,
    pub(super) remaining_word: u64,
}

impl<'a> Iterator for LogicalIter<'a> {
    type Item = &'a LogicalValue;
    fn next(&mut self) -> Option<Self::Item> {
        let value = match self.set {
            LogicalSet::Mutable(values) => values.get(self.position)?,
            LogicalSet::Frozen(row) => match &row.storage {
                FrozenLogicalStorage::Sparse8 { chunks, offsets } => {
                    let offset = *offsets.get(self.position)?;
                    if chunks
                        .get(self.chunk + 1)
                        .is_some_and(|&(_, start)| self.position == start as usize)
                    {
                        self.chunk += 1;
                    }
                    &row.values[(chunks[self.chunk].0 + u32::from(offset)) as usize]
                }
                FrozenLogicalStorage::Sparse { chunks, offsets } => {
                    let offset = *offsets.get(self.position)?;
                    if chunks
                        .get(self.chunk + 1)
                        .is_some_and(|&(_, start)| self.position == start as usize)
                    {
                        self.chunk += 1;
                    }
                    &row.values[(chunks[self.chunk].0 + u32::from(offset)) as usize]
                }
                FrozenLogicalStorage::Dense { words, .. } => {
                    while self.remaining_word == 0 {
                        self.remaining_word = *words.get(self.chunk)?;
                        self.chunk += 1;
                    }
                    let bit = self.remaining_word.trailing_zeros() as usize;
                    self.remaining_word &= self.remaining_word - 1;
                    &row.values[(self.chunk - 1) * 64 + bit]
                }
            },
        };
        self.position += 1;
        Some(value)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.set.len() - self.position;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for LogicalIter<'_> {}

impl FromIterator<LogicalValue> for LogicalSet {
    fn from_iter<T: IntoIterator<Item = LogicalValue>>(iter: T) -> Self {
        let mut values = iter.into_iter().collect::<Vec<_>>();
        values.sort_unstable();
        values.dedup();
        Self::Mutable(values)
    }
}

impl Extend<LogicalValue> for LogicalSet {
    fn extend<T: IntoIterator<Item = LogicalValue>>(&mut self, iter: T) {
        for value in iter {
            self.insert(value);
        }
    }
}

impl IntoIterator for LogicalSet {
    type Item = LogicalValue;
    type IntoIter = std::vec::IntoIter<LogicalValue>;
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Mutable(values) => values.into_iter(),
            Self::Frozen(_) => self.iter().copied().collect::<Vec<_>>().into_iter(),
        }
    }
}

impl<'a> IntoIterator for &'a LogicalSet {
    type Item = &'a LogicalValue;
    type IntoIter = LogicalIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
