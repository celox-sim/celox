//! Token-order indexes of the native arenas, built once per source file.
//!
//! Scope analyses select arena entries by token range. Visiting every entry of
//! the file for each scope makes a design shipped as one file quadratic, so
//! these indexes let a scope visit only the entries starting inside its own
//! ranges. Results keep arena order, so diagnostics are reported in the order
//! a whole-arena scan would report them.

use celox_sv_syntax::{Declarations, ExprId, TokenRange, TypeId};

/// Arena entries ordered by their first token.
#[derive(Debug)]
pub(crate) struct Starts<I = usize> {
    /// First token, arena position and handle of each entry.
    sorted: Vec<(usize, usize, I)>,
}

impl<I: Copy> Starts<I> {
    pub(crate) fn new(entries: impl Iterator<Item = (I, TokenRange)>) -> Self {
        let mut sorted = entries
            .enumerate()
            .map(|(index, (id, tokens))| (tokens.start, index, id))
            .collect::<Vec<_>>();
        sorted.sort_unstable_by_key(|(start, index, _)| (*start, *index));
        Self { sorted }
    }

    /// The entries whose first token lies in one of `ranges`, in arena
    /// order. Every entry a range contains is among them.
    pub(crate) fn within(&self, ranges: &[TokenRange]) -> Vec<I> {
        let mut entries = Vec::new();
        for range in merge(ranges) {
            let first = self
                .sorted
                .partition_point(|(start, ..)| *start < range.start);
            entries.extend(
                self.sorted[first..]
                    .iter()
                    .take_while(|(start, ..)| *start <= range.end)
                    .map(|(_, index, id)| (*index, *id)),
            );
        }
        entries.sort_unstable_by_key(|(index, _)| *index);
        entries.into_iter().map(|(_, id)| id).collect()
    }
}

/// Token ranges whose union is that of `ranges`, without overlaps.
fn merge(ranges: &[TokenRange]) -> Vec<TokenRange> {
    let mut sorted = ranges.to_vec();
    sorted.sort_unstable_by_key(|range| range.start);
    let mut merged = Vec::<TokenRange>::with_capacity(sorted.len());
    for range in sorted {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Token ranges answering which of them contain a range or a token.
#[derive(Debug, Default)]
pub(crate) struct Ranges {
    /// Ranges ordered by start with their position in the input, each with
    /// the largest end up to it.
    sorted: Vec<(TokenRange, usize, usize)>,
}

impl Ranges {
    pub(crate) fn new(ranges: impl IntoIterator<Item = TokenRange>) -> Self {
        let mut ranges = ranges.into_iter().enumerate().collect::<Vec<_>>();
        ranges.sort_by_key(|(_, range)| range.start);
        let mut end = 0;
        Self {
            sorted: ranges
                .into_iter()
                .map(|(index, range)| {
                    end = end.max(range.end);
                    (range, index, end)
                })
                .collect(),
        }
    }

    /// Input positions of the ranges for which `keep` holds, in input order.
    fn matching(&self, start: usize, keep: impl Fn(TokenRange) -> bool) -> Vec<usize> {
        let count = self
            .sorted
            .partition_point(|(range, ..)| range.start <= start);
        let mut indexes = self.sorted[..count]
            .iter()
            .rev()
            .take_while(|(.., reach)| keep(TokenRange { start, end: *reach }))
            .filter(|(range, ..)| keep(*range))
            .map(|(_, index, _)| *index)
            .collect::<Vec<_>>();
        indexes.sort_unstable();
        indexes
    }

    /// Input positions of the ranges that contain `inner`.
    pub(crate) fn containing(&self, inner: TokenRange) -> Vec<usize> {
        self.matching(inner.start, |range| inner.end <= range.end)
    }

    /// Input positions of the ranges that have token `at` before their end.
    pub(crate) fn covering(&self, at: usize) -> Vec<usize> {
        self.matching(at, |range| at < range.end)
    }

    /// The largest end of the ranges starting at or before `start`.
    fn reach(&self, start: usize) -> Option<usize> {
        let count = self
            .sorted
            .partition_point(|(range, ..)| range.start <= start);
        count.checked_sub(1).map(|last| self.sorted[last].2)
    }

    /// Whether a range contains `inner`.
    pub(crate) fn contains(&self, inner: TokenRange) -> bool {
        self.reach(inner.start).is_some_and(|end| inner.end <= end)
    }

    /// Whether a range has token `at` before its end.
    pub(crate) fn covers(&self, at: usize) -> bool {
        self.reach(at).is_some_and(|end| at < end)
    }
}

/// The arenas analyses select from by token range.
#[derive(Debug)]
pub(crate) struct Spans {
    pub expressions: Starts<ExprId>,
    pub types: Starts<TypeId>,
    pub body_items: Starts,
    pub net_declarations: Starts,
    pub instantiations: Starts,
    pub subroutines: Starts,
    /// Facts the scope validation derives from the whole file.
    pub validation: std::sync::OnceLock<crate::ast::validation::Facts>,
    /// Facts the import analysis derives from the whole file.
    pub imports: std::sync::OnceLock<crate::ast::imports::Facts>,
    pub(super) text: std::sync::OnceLock<SourceText>,
}

/// The expanded token text of a file, which identifies its source across
/// trees. Its hash is computed once, so lookups by it do not rehash the file.
#[derive(Clone, Debug)]
pub(crate) struct SourceText {
    hash: u64,
    text: std::sync::Arc<str>,
}

impl SourceText {
    pub(super) fn new(native: &Declarations) -> Self {
        use std::hash::{Hash, Hasher};
        let text = native
            .tokens
            .iter()
            .map(|token| token.text.as_str())
            .collect::<String>();
        let mut hasher = fxhash::FxHasher::default();
        text.hash(&mut hasher);
        Self {
            hash: hasher.finish(),
            text: text.into(),
        }
    }
}

impl PartialEq for SourceText {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.text, &other.text)
            || (self.hash == other.hash && self.text == other.text)
    }
}

impl Eq for SourceText {}

impl std::hash::Hash for SourceText {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl Spans {
    pub(super) fn new(native: &Declarations) -> Self {
        let indexed =
            |tokens: &mut dyn Iterator<Item = TokenRange>| Starts::new(tokens.enumerate());
        Self {
            expressions: Starts::new(
                native
                    .expression_nodes()
                    .map(|(id, node)| (id, node.tokens)),
            ),
            types: Starts::new(native.type_nodes().map(|(id, node)| (id, node.tokens))),
            body_items: indexed(&mut native.body_items.iter().map(|node| node.tokens)),
            net_declarations: indexed(&mut native.net_declarations.iter().map(|node| node.tokens)),
            instantiations: indexed(&mut native.instantiations.iter().map(|node| node.tokens)),
            subroutines: indexed(&mut native.subroutines.iter().map(|node| node.tokens)),
            validation: Default::default(),
            imports: Default::default(),
            text: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: usize, end: usize) -> TokenRange {
        TokenRange { start, end }
    }

    /// Ranges of every length over a short token sequence, overlapping,
    /// nested, empty and repeated.
    fn all_ranges() -> Vec<TokenRange> {
        (0..8)
            .flat_map(|start| (start..8).map(move |end| range(start, end)))
            .collect()
    }

    #[test]
    fn selections_match_whole_arena_scans() {
        let entries = all_ranges();
        let starts = Starts::new(entries.iter().copied().enumerate());
        let ranges = Ranges::new(entries.iter().copied());
        for scope in entries
            .iter()
            .flat_map(|a| entries.iter().map(move |b| [*a, *b]))
        {
            let contained = (0..entries.len())
                .filter(|index| {
                    scope.iter().any(|outer| {
                        outer.start <= entries[*index].start && entries[*index].end <= outer.end
                    })
                })
                .collect::<Vec<_>>();
            let within = starts.within(&scope);
            assert!(within.is_sorted(), "{scope:?}");
            assert!(
                contained.iter().all(|index| within.contains(index)),
                "{scope:?}"
            );
            assert!(
                within.iter().all(|index| scope
                    .iter()
                    .any(|outer| outer.start <= entries[*index].start
                        && entries[*index].start <= outer.end)),
                "{scope:?}"
            );
        }
        for inner in all_ranges() {
            let containing = (0..entries.len())
                .filter(|index| {
                    entries[*index].start <= inner.start && inner.end <= entries[*index].end
                })
                .collect::<Vec<_>>();
            assert_eq!(ranges.containing(inner), containing, "{inner:?}");
            assert_eq!(ranges.contains(inner), !containing.is_empty(), "{inner:?}");
        }
        for at in 0..9 {
            let covering = (0..entries.len())
                .filter(|index| entries[*index].start <= at && at < entries[*index].end)
                .collect::<Vec<_>>();
            assert_eq!(ranges.covering(at), covering, "{at}");
            assert_eq!(ranges.covers(at), !covering.is_empty(), "{at}");
        }
    }

    #[test]
    fn ranges_without_entries_select_nothing() {
        let ranges = Ranges::default();
        assert!(!ranges.contains(range(0, 0)));
        assert!(!ranges.covers(0));
        assert!(
            Starts::<usize>::new(std::iter::empty())
                .within(&[range(0, 9)])
                .is_empty()
        );
    }
}
