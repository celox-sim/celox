//! Full-block stack occupancy shares block endpoints and stores only block bits.
use std::sync::Arc;

use celox_backend_common::regalloc::{CompactSegments, LiveSegment};

use crate::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Blocks {
    Sparse(Vec<u32>),
    Dense { words: Vec<u64>, count: usize },
}

impl Default for Blocks {
    fn default() -> Self {
        Self::Sparse(Vec::new())
    }
}

impl Blocks {
    fn insert(&mut self, block: u32) {
        match self {
            Self::Sparse(blocks) => blocks.push(block),
            Self::Dense { words, count } => {
                let word = block as usize / 64;
                // A very high sparse identity must not expand a dense bitmap.
                if word >= words.len() && word < count.saturating_add(1) / 2 {
                    words.resize(word + 1, 0);
                }
                if word >= words.len() {
                    let mut blocks = self.iter().collect::<Vec<_>>();
                    blocks.push(block);
                    *self = Self::Sparse(blocks);
                    return;
                }
                let mask = 1 << (block % 64);
                *count += usize::from(words[word] & mask == 0);
                words[word] |= mask;
            }
        }
    }

    fn coalesce(&mut self) {
        let Self::Sparse(blocks) = self else { return };
        blocks.sort_unstable();
        blocks.dedup();
        let Some(&last) = blocks.last() else { return };
        let word_count = last as usize / 64 + 1;
        if word_count <= blocks.len() / 2 {
            let mut words = vec![0; word_count];
            for &block in blocks.iter() {
                words[block as usize / 64] |= 1 << (block % 64);
            }
            *self = Self::Dense {
                words,
                count: blocks.len(),
            };
        } else {
            blocks.shrink_to_fit();
        }
    }

    fn contains(&self, block: u32) -> bool {
        match self {
            Self::Sparse(blocks) => blocks.binary_search(&block).is_ok(),
            Self::Dense { words, .. } => words
                .get(block as usize / 64)
                .is_some_and(|word| word & (1 << (block % 64)) != 0),
        }
    }

    fn remove(&mut self, block: u32) {
        match self {
            Self::Sparse(blocks) => {
                if let Ok(index) = blocks.binary_search(&block) {
                    blocks.remove(index);
                }
            }
            Self::Dense { words, count } => {
                if let Some(word) = words.get_mut(block as usize / 64) {
                    let mask = 1 << (block % 64);
                    *count -= usize::from(*word & mask != 0);
                    *word &= !mask;
                }
            }
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Sparse(blocks) => blocks.len(),
            Self::Dense { count, .. } => *count,
        }
    }

    fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let mut position = 0;
        let mut remaining = match self {
            Self::Dense { words, .. } => words.first().copied().unwrap_or(0),
            Self::Sparse(_) => 0,
        };
        std::iter::from_fn(move || match self {
            Self::Sparse(blocks) => {
                let block = blocks.get(position).copied()?;
                position += 1;
                Some(block)
            }
            Self::Dense { words, .. } => {
                while remaining == 0 {
                    position += 1;
                    remaining = *words.get(position)?;
                }
                let bit = remaining.trailing_zeros();
                remaining &= remaining - 1;
                Some(position as u32 * 64 + bit)
            }
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HomeRanges {
    ends: Arc<HashMap<u32, u64>>,
    full: Blocks,
    partial: CompactSegments,
}

impl HomeRanges {
    pub(super) fn new(ends: &Arc<HashMap<u32, u64>>) -> Self {
        Self {
            ends: Arc::clone(ends),
            ..Self::default()
        }
    }

    pub(super) fn extend(&mut self, segments: impl IntoIterator<Item = LiveSegment>) {
        for segment in segments {
            if segment.start == 0
                && u32::try_from(segment.block)
                    .ok()
                    .and_then(|block| self.ends.get(&block))
                    == Some(&segment.end)
            {
                self.full.insert(segment.block as u32);
            } else {
                self.partial.extend([segment]);
            }
        }
    }

    pub(super) fn coalesce(&mut self) {
        self.partial.coalesce();
        // Adjacent partial intervals can now span a complete block.
        let partial = std::mem::take(&mut self.partial);
        self.extend(partial.into_vec());
        self.full.coalesce();
        self.partial = self
            .partial
            .iter()
            .filter_map(|mut segment| {
                if let Ok(block) = u32::try_from(segment.block)
                    && self.full.contains(block)
                {
                    let end = self.ends[&block];
                    if segment.end <= end {
                        return None;
                    }
                    if segment.start <= end {
                        self.full.remove(block);
                        segment.start = 0;
                    }
                }
                Some(segment)
            })
            .collect();
        self.partial.coalesce();
    }

    pub(super) fn len(&self) -> usize {
        self.full.len() + self.partial.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // Consumers iterate only finalized/coalesced rows.
    pub(super) fn iter(&self) -> impl Iterator<Item = LiveSegment> + '_ {
        let mut full = self.full.iter().peekable();
        let mut partial = self.partial.iter().peekable();
        std::iter::from_fn(move || {
            if full.peek().is_some_and(|&block| {
                partial
                    .peek()
                    .is_none_or(|segment| block as usize <= segment.block)
            }) {
                let block = full.next()?;
                Some(LiveSegment {
                    block: block as usize,
                    start: 0,
                    end: self.ends[&block],
                })
            } else {
                partial.next()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_blocks_match_compact_union_across_batches_and_sparse_ids() {
        for base in [0, u32::MAX - 255] {
            let ends = Arc::new(
                (0..256)
                    .map(|i| (base + i, 10 + u64::from(i % 5)))
                    .collect(),
            );
            let source = (0..256)
                .flat_map(|i| {
                    let block = (base + i) as usize;
                    let end = 10 + u64::from(i % 5);
                    [
                        LiveSegment {
                            block,
                            start: 0,
                            end: 5,
                        },
                        LiveSegment {
                            block,
                            start: 5,
                            end,
                        },
                        LiveSegment {
                            block,
                            start: 2,
                            end: 7,
                        },
                    ]
                })
                .collect::<Vec<_>>();
            let mut reference: CompactSegments = source.iter().copied().collect();
            reference.coalesce();
            for batch in [1, 7, 64, 768] {
                let mut actual = HomeRanges::new(&ends);
                for chunk in source.chunks(batch) {
                    actual.extend(chunk.iter().copied());
                    actual.coalesce();
                }
                assert_eq!(
                    actual.iter().collect::<Vec<_>>(),
                    reference.iter().collect::<Vec<_>>()
                );
                assert_eq!(actual.len(), reference.len());
                assert!(actual.partial.is_empty());
                assert_eq!(matches!(actual.full, Blocks::Dense { .. }), base == 0);
            }
        }
    }

    #[test]
    fn full_and_partial_union_is_exact_even_outside_shared_endpoints() {
        let ends = Arc::new([(0, 10), (64, 10), (u32::MAX, 10)].into_iter().collect());
        for block in [0, 64, u32::MAX] {
            for (start, end) in [(0, 20), (5, 20), (10, 20), (11, 20)] {
                let segments = [
                    LiveSegment {
                        block: block as usize,
                        start: 0,
                        end: 10,
                    },
                    LiveSegment {
                        block: block as usize,
                        start,
                        end,
                    },
                ];
                let mut expected: CompactSegments = segments.into_iter().collect();
                expected.coalesce();
                let mut actual = HomeRanges::new(&ends);
                for segment in segments {
                    actual.extend([segment]);
                    actual.coalesce();
                }
                assert_eq!(
                    actual.iter().collect::<Vec<_>>(),
                    expected.iter().collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn partial_holes_and_noncanonical_endpoints_remain_exact() {
        let ends = Arc::new([(3, 100), (7, u64::MAX)].into_iter().collect());
        let segments = [
            LiveSegment {
                block: 3,
                start: 0,
                end: 20,
            },
            LiveSegment {
                block: 3,
                start: 30,
                end: 100,
            },
            LiveSegment {
                block: 7,
                start: 0,
                end: u64::MAX,
            },
            LiveSegment {
                block: 11,
                start: 0,
                end: 9,
            },
        ];
        let mut actual = HomeRanges::new(&ends);
        actual.extend(segments);
        actual.coalesce();
        assert_eq!(actual.iter().collect::<Vec<_>>(), segments);
    }
}
