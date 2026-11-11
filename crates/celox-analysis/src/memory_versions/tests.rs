use super::*;
use crate::cfg::ControlFlowGraph;
use crate::memory::{MemoryEffect, MemoryLocation, effects_may_alias};
use crate::memory_ssa::{self, ClobberWalker, MemoryAccess, MemoryAccessEvent, MemoryClobber};
use crate::ssa::{self, Event, Version};

/// `(block, event index)` identity of a definition.
type Definition = (usize, usize);

struct Program {
    events: Vec<Vec<MemoryVersionEvent<Definition>>>,
    writes: Vec<RangeWrite>,
}

impl Program {
    fn new(blocks: usize) -> Self {
        Self {
            events: vec![Vec::new(); blocks],
            writes: Vec::new(),
        }
    }

    fn point(&mut self, block: usize) -> usize {
        let index = self.events[block].len();
        self.events[block].push(MemoryVersionEvent {
            definition: None,
            write_start: self.writes.len(),
            write_end: self.writes.len(),
        });
        index
    }

    fn write(&mut self, block: usize, ranges: &[RangeWrite]) -> usize {
        let index = self.events[block].len();
        let write_start = self.writes.len();
        self.writes.extend_from_slice(ranges);
        self.events[block].push(MemoryVersionEvent {
            definition: Some((block, index)),
            write_start,
            write_end: self.writes.len(),
        });
        index
    }

    fn build(&self, cfg: &ControlFlowGraph) -> MemoryVersions<Definition> {
        build(cfg, &self.events, &self.writes).unwrap()
    }
}

fn exact(start: i64, end: i64) -> RangeWrite {
    RangeWrite::Exact { start, end }
}

fn versions_over(
    analysis: &MemoryVersions<Definition>,
    version: VersionId,
    start: i64,
    end: i64,
) -> Vec<(i64, i64, MemoryVersion<Definition>)> {
    let mut segments = Vec::new();
    analysis
        .segments(version, start, end, &mut segments)
        .unwrap();
    segments
        .into_iter()
        .map(|segment| (segment.start, segment.end, segment.version))
        .collect()
}

#[test]
fn straight_line_writes_version_exactly_their_bytes() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let mut program = Program::new(1);
    program.write(0, &[exact(8, 16)]);
    program.write(0, &[exact(12, 14), exact(100, 101)]);
    let point = program.point(0);
    let analysis = program.build(&cfg);

    let (before, after) = analysis.event(0, point).unwrap();
    assert_eq!(before, after);
    assert_eq!(
        versions_over(&analysis, after, 0, 20),
        vec![
            (0, 8, MemoryVersion::LiveOnEntry),
            (8, 12, MemoryVersion::Definition((0, 0))),
            (12, 14, MemoryVersion::Definition((0, 1))),
            (14, 16, MemoryVersion::Definition((0, 0))),
            (16, 20, MemoryVersion::LiveOnEntry),
        ]
    );
    assert_eq!(
        analysis.version_at(after, 100),
        Some(MemoryVersion::Definition((0, 1)))
    );
    assert_eq!(analysis.phi_range_count(), 0);
    assert_eq!(analysis.definition_count(), 2);
}

#[test]
fn unknown_write_covers_every_byte() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let mut program = Program::new(1);
    program.write(0, &[exact(8, 16)]);
    program.write(0, &[RangeWrite::All]);
    let analysis = program.build(&cfg);
    assert_eq!(
        versions_over(
            &analysis,
            analysis.block_exit(0).unwrap(),
            i64::MIN,
            i64::MAX
        ),
        vec![(i64::MIN, i64::MAX, MemoryVersion::Definition((0, 1)))]
    );
}

#[test]
fn join_phi_exists_only_where_predecessors_disagree() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1, 2], vec![3], vec![3], vec![]], 0).unwrap();
    let mut program = Program::new(4);
    program.write(0, &[exact(0, 8)]);
    program.write(1, &[exact(4, 12)]);
    let analysis = program.build(&cfg);

    assert_eq!(analysis.phi_ranges(3), vec![(4, 12)]);
    assert_eq!(
        versions_over(&analysis, analysis.block_entry(3).unwrap(), 0, 16),
        vec![
            (0, 4, MemoryVersion::Definition((0, 0))),
            (4, 12, MemoryVersion::Phi { block: 3 }),
            (12, 16, MemoryVersion::LiveOnEntry),
        ]
    );
    assert_eq!(
        versions_over(&analysis, analysis.block_exit(1).unwrap(), 4, 12),
        vec![(4, 12, MemoryVersion::Definition((1, 0)))]
    );
    assert_eq!(
        versions_over(&analysis, analysis.block_exit(2).unwrap(), 4, 12),
        vec![
            (4, 8, MemoryVersion::Definition((0, 0))),
            (8, 12, MemoryVersion::LiveOnEntry),
        ]
    );
}

#[test]
fn loop_header_phi_exists_only_for_ranges_written_in_the_loop() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1], vec![1, 2], vec![]], 0).unwrap();
    let mut program = Program::new(3);
    program.write(0, &[exact(0, 16)]);
    program.write(1, &[exact(8, 12)]);
    let analysis = program.build(&cfg);

    assert_eq!(analysis.phi_ranges(1), vec![(8, 12)]);
    assert_eq!(
        versions_over(&analysis, analysis.block_entry(1).unwrap(), 0, 16),
        vec![
            (0, 8, MemoryVersion::Definition((0, 0))),
            (8, 12, MemoryVersion::Phi { block: 1 }),
            (12, 16, MemoryVersion::Definition((0, 0))),
        ]
    );
    assert_eq!(
        analysis.version_at(analysis.block_entry(2).unwrap(), 8),
        Some(MemoryVersion::Definition((1, 0)))
    );
}

/// The shape behind celox-sim/celox#2041: a join inside a loop whose
/// write does not touch the queried range.  No version of that range is
/// introduced anywhere in the loop.
#[test]
fn disjoint_write_in_a_loop_diamond_introduces_no_version() {
    let cfg = ControlFlowGraph::analyze(
        vec![vec![1], vec![2, 3], vec![4], vec![4], vec![1, 5], vec![]],
        0,
    )
    .unwrap();
    let mut program = Program::new(6);
    program.write(2, &[exact(64, 72)]);
    let analysis = program.build(&cfg);

    assert_eq!(analysis.phi_ranges(1), vec![(64, 72)]);
    assert_eq!(analysis.phi_ranges(4), vec![(64, 72)]);
    for block in 0..6 {
        assert_eq!(
            versions_over(&analysis, analysis.block_entry(block).unwrap(), 0, 8),
            vec![(0, 8, MemoryVersion::LiveOnEntry)],
            "block {block}"
        );
    }
    assert_eq!(
        versions_over(&analysis, analysis.block_entry(4).unwrap(), 64, 72),
        vec![(64, 72, MemoryVersion::Phi { block: 4 })]
    );
    assert_eq!(
        versions_over(&analysis, analysis.block_exit(3).unwrap(), 64, 72),
        vec![(64, 72, MemoryVersion::Phi { block: 1 })]
    );
}

#[test]
fn irreducible_cycle_receives_phis_at_both_entries() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1, 2], vec![2], vec![1, 3], vec![]], 0).unwrap();
    let mut program = Program::new(4);
    program.write(1, &[exact(0, 4)]);
    let analysis = program.build(&cfg);

    assert_eq!(analysis.phi_ranges(1), vec![(0, 4)]);
    assert_eq!(analysis.phi_ranges(2), vec![(0, 4)]);
    assert_eq!(
        analysis.version_at(analysis.block_exit(1).unwrap(), 0),
        Some(MemoryVersion::Definition((1, 0)))
    );
    assert_eq!(
        analysis.version_at(analysis.block_entry(3).unwrap(), 0),
        Some(MemoryVersion::Phi { block: 2 })
    );
}

#[test]
fn root_with_incoming_edges_and_writes_is_rejected() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1], vec![0, 2], vec![]], 0).unwrap();
    let mut program = Program::new(3);
    program.write(1, &[exact(0, 4)]);
    let error = build(&cfg, &program.events, &program.writes).unwrap_err();
    assert_eq!(error.rule, "MEMORY_VERSIONS.ROOT_PHI");
}

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

fn random_program(random: &mut Xorshift, blocks: usize, allow_unknown: bool) -> Program {
    let mut program = Program::new(blocks);
    for block in 0..blocks {
        for _ in 0..random.next(4) {
            match random.next(4) {
                0 => {
                    program.point(block);
                }
                1 if allow_unknown && random.next(6) == 0 => {
                    program.write(block, &[RangeWrite::All]);
                }
                _ => {
                    let ranges = (0..1 + random.next(2))
                        .map(|_| {
                            let start = 4 * random.next(4) as i64;
                            exact(start, start + 4 * (1 + random.next(2)) as i64)
                        })
                        .collect::<Vec<_>>();
                    program.write(block, &ranges);
                }
            }
        }
    }
    program
}

/// Random reachable CFGs without edges into the root.  Successor choice is
/// unrestricted, so the graphs contain loops, nested loops and irreducible
/// cycles.
fn random_cfg(random: &mut Xorshift, acyclic: bool) -> Option<(usize, ControlFlowGraph)> {
    let blocks = 1 + random.next(7);
    let successors = (0..blocks)
        .map(|block| {
            let mut outgoing = Vec::new();
            if block + 1 < blocks {
                outgoing.push(block + 1);
            }
            for _ in 0..random.next(3) {
                let target = if acyclic {
                    if block + 1 >= blocks {
                        continue;
                    }
                    block + 1 + random.next(blocks - block - 1)
                } else {
                    1 + random.next(blocks.max(2) - 1)
                };
                if target < blocks {
                    outgoing.push(target);
                }
            }
            outgoing
        })
        .collect::<Vec<_>>();
    ControlFlowGraph::analyze(successors, 0)
        .ok()
        .map(|cfg| (blocks, cfg))
}

/// Per-atom oracle: one `ssa::build` variable per atom, with uses at every
/// block boundary and event.
#[test]
fn random_cfgs_match_one_sparse_ssa_per_atom() {
    let mut random = Xorshift(0x9e37_79b9_7f4a_7c15);
    let mut cases = 0;
    let mut phi_cases = 0;
    for _ in 0..600 {
        let Some((blocks, cfg)) = random_cfg(&mut random, false) else {
            continue;
        };
        let program = random_program(&mut random, blocks, true);
        let analysis = program.build(&cfg);

        let mut boundaries = program
            .writes
            .iter()
            .filter_map(|write| match *write {
                RangeWrite::Exact { start, end } => Some([start, end]),
                RangeWrite::All => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        boundaries.sort_unstable();
        boundaries.dedup();
        let atoms = boundaries.len() + 1;
        let atom_byte = |atom: usize| {
            if atom == 0 {
                boundaries.first().map_or(0, |first| first - 1)
            } else {
                boundaries[atom - 1]
            }
        };
        let covers = |write: RangeWrite, byte: i64| match write {
            RangeWrite::Exact { start, end } => start <= byte && byte < end,
            RangeWrite::All => true,
        };

        // One SSA variable per atom; use identities are
        // (block, slot) with slot 0 = entry, 2k+1/2k+2 = before/after
        // event k, last = exit.
        let mut events = vec![Vec::<Event<usize, Definition, (usize, usize)>>::new(); blocks];
        for (block, block_events) in program.events.iter().enumerate() {
            for atom in 0..atoms {
                events[block].push(Event::Use {
                    variable: atom,
                    usage: (block, 0),
                });
            }
            for (index, event) in block_events.iter().enumerate() {
                for atom in 0..atoms {
                    events[block].push(Event::Use {
                        variable: atom,
                        usage: (block, 2 * index + 1),
                    });
                }
                if let Some(definition) = event.definition {
                    for atom in 0..atoms {
                        if program.writes[event.write_start..event.write_end]
                            .iter()
                            .any(|&write| covers(write, atom_byte(atom)))
                        {
                            events[block].push(Event::Definition {
                                variable: atom,
                                definition,
                            });
                        }
                    }
                }
                for atom in 0..atoms {
                    events[block].push(Event::Use {
                        variable: atom,
                        usage: (block, 2 * index + 2),
                    });
                }
            }
            for atom in 0..atoms {
                events[block].push(Event::Use {
                    variable: atom,
                    usage: (block, 2 * block_events.len() + 1),
                });
            }
        }
        // Use identities must be unique per variable, which the single
        // shared usage key is not: tag them with the atom.
        for block_events in &mut events {
            for event in block_events.iter_mut() {
                if let Event::Use { variable, usage } = event {
                    *usage = (usage.0, usage.1 * atoms + *variable);
                }
            }
        }
        let oracle = ssa::build(&cfg, &events).unwrap();
        let convert = |version: Version<usize, Definition>| match version {
            Version::Entry(_) => MemoryVersion::LiveOnEntry,
            Version::Definition { definition, .. } => MemoryVersion::Definition(definition),
            Version::Phi { block, .. } => MemoryVersion::Phi { block },
        };

        for block in 0..blocks {
            let slots = program.events[block].len();
            for slot in 0..=2 * slots + 1 {
                let version = if slot == 0 {
                    analysis.block_entry(block).unwrap()
                } else if slot == 2 * slots + 1 {
                    analysis.block_exit(block).unwrap()
                } else {
                    let (before, after) = analysis.event(block, (slot - 1) / 2).unwrap();
                    if slot % 2 == 1 { before } else { after }
                };
                for atom in 0..atoms {
                    let expected = convert(oracle.uses[&(block, slot * atoms + atom)]);
                    let actual = analysis.version_at(version, atom_byte(atom)).unwrap();
                    assert_eq!(
                        actual, expected,
                        "block {block} slot {slot} atom {atom} {:?} {:?}",
                        program.events, program.writes
                    );
                }
            }
        }
        // Phi inputs are the predecessors' exit versions.
        for phi in &oracle.phis {
            phi_cases += 1;
            let byte = atom_byte(phi.variable);
            assert_eq!(
                analysis.version_at(analysis.block_entry(phi.block).unwrap(), byte),
                Some(MemoryVersion::Phi { block: phi.block })
            );
            for &(predecessor, version) in &phi.inputs {
                assert_eq!(
                    analysis.version_at(analysis.block_exit(predecessor).unwrap(), byte),
                    Some(convert(version))
                );
            }
        }
        // And the analysis has no phi the oracle lacks.
        let mut oracle_phi_bytes = 0usize;
        for phi in &oracle.phis {
            let _ = phi;
            oracle_phi_bytes += 1;
        }
        let mut analysis_phi_atoms = 0usize;
        for block in 0..blocks {
            for (start, end) in analysis.phi_ranges(block) {
                analysis_phi_atoms += (0..atoms)
                    .filter(|&atom| start <= atom_byte(atom) && atom_byte(atom) < end)
                    .count();
            }
        }
        assert_eq!(analysis_phi_atoms, oracle_phi_bytes);
        cases += 1;
    }
    assert!(cases > 300, "{cases}");
    assert!(phi_cases > 300, "{phi_cases}");
}

struct WalkerAnalysis {
    graph: memory_ssa::MemoryAccessGraph<Definition>,
    points: memory_ssa::MemoryPointMap<(usize, usize)>,
    writes: BTreeMap<Definition, Vec<MemoryEffect<u8>>>,
}

fn walker_analysis(cfg: &ControlFlowGraph, program: &Program) -> WalkerAnalysis {
    let mut events = vec![Vec::new(); program.events.len()];
    let mut writes = BTreeMap::new();
    for (block, block_events) in program.events.iter().enumerate() {
        for (index, event) in block_events.iter().enumerate() {
            if let Some(definition) = event.definition {
                writes.insert(
                    definition,
                    program.writes[event.write_start..event.write_end]
                        .iter()
                        .map(|write| match *write {
                            RangeWrite::Exact { start, end } => {
                                MemoryEffect::Exact(MemoryLocation {
                                    object: 1u8,
                                    offset: start,
                                    byte_len: (end - start) as usize,
                                })
                            }
                            RangeWrite::All => MemoryEffect::UnknownObject(1u8),
                        })
                        .collect::<Vec<_>>(),
                );
            }
            events[block].push(MemoryAccessEvent {
                point: (block, index),
                definition: event.definition,
            });
        }
    }
    let (graph, points) = memory_ssa::build(cfg, &events).unwrap();
    WalkerAnalysis {
        graph,
        points,
        writes,
    }
}

impl WalkerAnalysis {
    fn clobber(
        &self,
        walker: &mut ClobberWalker,
        start: memory_ssa::MemoryAccessId,
        offset: i64,
        byte_len: usize,
    ) -> MemoryClobber {
        let query = MemoryEffect::Exact(MemoryLocation {
            object: 1u8,
            offset,
            byte_len,
        });
        let mut oracle = |definition: &Definition, query: &MemoryEffect<u8>| {
            self.writes[definition]
                .iter()
                .any(|&write| effects_may_alias(write, *query))
        };
        walker
            .clobber(&self.graph, start, &query, &mut oracle)
            .unwrap()
    }
}

/// On acyclic CFGs the clobber walker is exact, and two points have the
/// same nearest clobber for a range exactly when every byte of the range
/// has the same version at both.
#[test]
fn acyclic_cfgs_agree_with_the_clobber_walker_on_snapshot_equality() {
    let mut random = Xorshift(0x2545_f491_4f6c_dd1d);
    let mut cases = 0;
    let mut equal_pairs = 0;
    for _ in 0..600 {
        let Some((blocks, cfg)) = random_cfg(&mut random, true) else {
            continue;
        };
        let program = random_program(&mut random, blocks, true);
        let analysis = program.build(&cfg);
        let walker_analysis = walker_analysis(&cfg, &program);
        let mut walker = ClobberWalker::new();

        let mut points = Vec::new();
        for block in 0..blocks {
            points.push((
                analysis.block_entry(block).unwrap(),
                walker_analysis.points.block_entry(block).unwrap(),
            ));
            points.push((
                analysis.block_exit(block).unwrap(),
                walker_analysis.points.block_exit(block).unwrap(),
            ));
            for index in 0..program.events[block].len() {
                let (before, after) = analysis.event(block, index).unwrap();
                let access = walker_analysis.points.event((block, index)).unwrap();
                points.push((before, access.before));
                points.push((after, access.after));
            }
        }
        for _ in 0..6 {
            let offset = 4 * random.next(4) as i64;
            let byte_len = 4 * (1 + random.next(2));
            let certificates = points
                .iter()
                .map(|&(version, access)| {
                    let mut segments = Vec::new();
                    analysis
                        .segments(version, offset, offset + byte_len as i64, &mut segments)
                        .unwrap();
                    let MemoryClobber::Access(clobber) =
                        walker_analysis.clobber(&mut walker, access, offset, byte_len)
                    else {
                        panic!("acyclic graphs have no indeterminate clobbers")
                    };
                    (segments, clobber)
                })
                .collect::<Vec<_>>();
            for (left, (left_segments, left_clobber)) in certificates.iter().enumerate() {
                for (right_segments, right_clobber) in &certificates[left..] {
                    let walker_equal = left_clobber == right_clobber;
                    let versions_equal = left_segments == right_segments;
                    assert_eq!(
                        walker_equal, versions_equal,
                        "{offset}+{byte_len}: {left_segments:?} / {left_clobber:?} vs {right_segments:?} / {right_clobber:?} in {:?} {:?}",
                        program.events, program.writes
                    );
                    equal_pairs += usize::from(walker_equal);
                }
            }
            // A write clobber versions exactly the bytes it writes.
            for (segments, clobber) in &certificates {
                if let Some(MemoryAccess::Definition { definition, .. }) =
                    walker_analysis.graph.access(*clobber)
                {
                    assert!(segments.iter().any(|segment| {
                        segment.version == MemoryVersion::Definition(*definition)
                    }));
                }
            }
        }
        cases += 1;
    }
    assert!(cases > 300, "{cases}");
    assert!(equal_pairs > 1000, "{equal_pairs}");
}

/// Documented divergence from the clobber walker (celox-sim/celox#2041).
///
/// Loop `1 -> {2, 3} -> 4 -> 1` writes bytes `64..72` in block 2 only.
/// For a query of bytes `0..8` nothing in the loop aliases, so the
/// version at every block is live-on-entry.  The walker started at the
/// header's MemoryPhi finds every input of the inner join's MemoryPhi on
/// its own stack, keeps that phi as a version of its own, and therefore
/// also keeps the header phi; started at the inner join it folds both.
/// The certificate thus depends on the start of the walk, which is what
/// made planner and rebuild disagree.
#[test]
fn clobber_walker_cycle_rule_is_not_canonical_on_loops() {
    let cfg = ControlFlowGraph::analyze(
        vec![vec![1], vec![2, 3], vec![4], vec![4], vec![1, 5], vec![]],
        0,
    )
    .unwrap();
    let mut program = Program::new(6);
    program.write(2, &[exact(64, 72)]);
    let analysis = program.build(&cfg);
    let walker_analysis = walker_analysis(&cfg, &program);

    let header = walker_analysis.points.block_entry(1).unwrap();
    let join = walker_analysis.points.block_entry(4).unwrap();
    let classify = |clobber| match clobber {
        MemoryClobber::Indeterminate => None,
        MemoryClobber::Access(access) => Some(match walker_analysis.graph.access(access) {
            Some(MemoryAccess::LiveOnEntry) => MemoryVersion::LiveOnEntry,
            Some(MemoryAccess::Definition { definition, .. }) => {
                MemoryVersion::Definition(*definition)
            }
            Some(MemoryAccess::Phi { block, .. }) => MemoryVersion::Phi { block },
            None => unreachable!(),
        }),
    };
    let mut walker = ClobberWalker::new();
    let from_header = classify(walker_analysis.clobber(&mut walker, header, 0, 8));
    let mut walker = ClobberWalker::new();
    let from_join = classify(walker_analysis.clobber(&mut walker, join, 0, 8));
    assert_eq!(from_header, Some(MemoryVersion::Phi { block: 1 }));
    assert_eq!(from_join, Some(MemoryVersion::LiveOnEntry));

    for block in [1, 4] {
        assert_eq!(
            analysis.version_at(analysis.block_entry(block).unwrap(), 0),
            Some(MemoryVersion::LiveOnEntry)
        );
    }
}
