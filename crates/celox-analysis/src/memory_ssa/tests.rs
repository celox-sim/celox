use super::*;
use crate::cfg::ControlFlowGraph;
use crate::memory::{MemoryEffect, MemoryLocation, effects_may_alias};

type Object = u8;
type Instruction = (usize, usize);

#[derive(Debug)]
struct EffectEvent {
    instruction: Instruction,
    reads: Vec<MemoryEffect<Object>>,
    writes: Vec<MemoryEffect<Object>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestClobber {
    LiveOnEntry,
    Definition(Instruction),
    Phi(usize),
    Indeterminate,
}

struct Analysis {
    graph: MemoryAccessGraph<Instruction>,
    points: MemoryPointMap<Instruction>,
    writes: BTreeMap<Instruction, Vec<MemoryEffect<Object>>>,
    reads: Vec<(Instruction, usize, MemoryEffect<Object>)>,
}

impl Analysis {
    fn clobber(&self, instruction: Instruction, query: MemoryEffect<Object>) -> TestClobber {
        let start = self.points.event(instruction).unwrap().before;
        let mut oracle = |definition: &Instruction, query: &MemoryEffect<Object>| {
            self.writes[definition]
                .iter()
                .copied()
                .any(|write| effects_may_alias(write, *query))
        };
        let mut walker = ClobberWalker::new();
        match walker
            .clobber(&self.graph, start, &query, &mut oracle)
            .unwrap()
        {
            MemoryClobber::Indeterminate => TestClobber::Indeterminate,
            MemoryClobber::Access(access) => match self.graph.access(access).unwrap() {
                MemoryAccess::LiveOnEntry => TestClobber::LiveOnEntry,
                MemoryAccess::Definition { definition, .. } => TestClobber::Definition(*definition),
                MemoryAccess::Phi { block, .. } => TestClobber::Phi(block),
            },
        }
    }

    fn only_clobber(&self) -> TestClobber {
        assert_eq!(self.reads.len(), 1);
        let (instruction, _, query) = self.reads[0];
        self.clobber(instruction, query)
    }
}

fn exact(object: Object, offset: i64, byte_len: usize) -> MemoryEffect<Object> {
    MemoryEffect::Exact(MemoryLocation {
        object,
        offset,
        byte_len,
    })
}

fn event(
    instruction: Instruction,
    reads: Vec<MemoryEffect<Object>>,
    writes: Vec<MemoryEffect<Object>>,
) -> EffectEvent {
    EffectEvent {
        instruction,
        reads,
        writes,
    }
}

fn analyze(cfg: &ControlFlowGraph, events: &[Vec<EffectEvent>]) -> Analysis {
    let mut access_events = vec![Vec::new(); events.len()];
    let mut writes = BTreeMap::new();
    let mut reads = Vec::new();
    for (block, block_events) in events.iter().enumerate() {
        for event in block_events {
            for (read_index, &read) in event.reads.iter().enumerate() {
                reads.push((event.instruction, read_index, read));
            }
            if !event.writes.is_empty() {
                writes.insert(event.instruction, event.writes.clone());
            }
            access_events[block].push(MemoryAccessEvent {
                point: event.instruction,
                definition: (!event.writes.is_empty()).then_some(event.instruction),
            });
        }
    }
    let (graph, points) = build(cfg, &access_events).unwrap();
    Analysis {
        graph,
        points,
        writes,
        reads,
    }
}

#[test]
fn exact_store_reaches_later_load() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[vec![
            event((0, 0), vec![], vec![exact(1, 8, 4)]),
            event((0, 1), vec![exact(1, 8, 4)], vec![]),
        ]],
    );

    assert_eq!(analysis.only_clobber(), TestClobber::Definition((0, 0)));
    assert_eq!(analysis.graph.definition_count(), 1);
    assert_eq!(analysis.graph.phi_count(), 0);
    assert_eq!(analysis.graph.access_count(), 2);
}

#[test]
fn point_map_supports_lowering_coordinates_without_owning_queries() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let analysis = analyze(&cfg, &[vec![event((0, 0), vec![], vec![exact(1, 8, 8)])]]);
    let event = analysis.points.event((0, 0)).unwrap();

    assert_eq!(
        analysis.clobber((0, 0), exact(1, 8, 8)),
        TestClobber::LiveOnEntry
    );
    let mut oracle = |definition: &Instruction, query: &MemoryEffect<Object>| {
        analysis.writes[definition]
            .iter()
            .copied()
            .any(|write| effects_may_alias(write, *query))
    };
    let mut walker = ClobberWalker::new();
    let after = walker
        .clobber(&analysis.graph, event.after, &exact(1, 8, 8), &mut oracle)
        .unwrap();
    let MemoryClobber::Access(after) = after else {
        panic!("a store's post-state has a concrete clobber")
    };
    assert!(matches!(
        analysis.graph.access(after),
        Some(MemoryAccess::Definition {
            definition: &(0, 0),
            ..
        })
    ));
    assert_eq!(analysis.points.block_entry(0), Some(event.before));
    assert_eq!(analysis.points.block_exit(0), Some(event.after));
}

#[test]
fn disjoint_store_is_skipped_by_the_external_alias_oracle() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[vec![
            event((0, 0), vec![], vec![exact(1, 0, 8)]),
            event((0, 1), vec![exact(1, 16, 8)], vec![]),
        ]],
    );

    assert_eq!(analysis.only_clobber(), TestClobber::LiveOnEntry);
}

#[test]
fn partial_overlap_and_unknown_object_are_clobbers() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let overlap = analyze(
        &cfg,
        &[vec![
            event((0, 0), vec![], vec![exact(1, 4, 8)]),
            event((0, 1), vec![exact(1, 8, 8)], vec![]),
        ]],
    );
    assert_eq!(overlap.only_clobber(), TestClobber::Definition((0, 0)));

    let unknown = analyze(
        &cfg,
        &[vec![
            event((0, 0), vec![], vec![MemoryEffect::UnknownObject(1)]),
            event((0, 1), vec![exact(1, 8, 8)], vec![]),
        ]],
    );
    assert_eq!(unknown.only_clobber(), TestClobber::Definition((0, 0)));
}

#[test]
fn one_arm_store_resolves_to_the_join_phi() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1, 2], vec![3], vec![3], vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[
            vec![],
            vec![event((1, 0), vec![], vec![exact(1, 8, 8)])],
            vec![],
            vec![event((3, 0), vec![exact(1, 8, 8)], vec![])],
        ],
    );

    assert_eq!(analysis.only_clobber(), TestClobber::Phi(3));
    assert_eq!(analysis.graph.phi_count(), 1);
}

#[test]
fn one_query_session_reuses_phi_input_clobbers() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1, 2], vec![3], vec![3], vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[
            vec![],
            vec![event((1, 0), vec![], vec![exact(1, 8, 8)])],
            vec![],
            vec![event((3, 0), vec![exact(1, 8, 8)], vec![])],
        ],
    );
    let query = exact(1, 8, 8);
    let oracle_calls = |starts: &[MemoryAccessId]| {
        let mut calls = 0usize;
        let mut oracle = |definition: &Instruction, query: &MemoryEffect<Object>| {
            calls += 1;
            analysis.writes[definition]
                .iter()
                .copied()
                .any(|write| effects_may_alias(write, *query))
        };
        let mut walker = ClobberWalker::new();
        let mut session = walker.query(&analysis.graph, &query, &mut oracle);
        for &start in starts {
            assert!(matches!(
                session.clobber(start),
                Some(MemoryClobber::Access(_))
            ));
        }
        calls
    };
    let join = analysis.points.event((3, 0)).unwrap().before;
    let left_exit = analysis.points.block_exit(1).unwrap();
    assert_eq!(oracle_calls(&[join, left_exit]), oracle_calls(&[join]));
}

#[test]
fn same_dominating_store_is_found_through_a_join() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1, 2], vec![3], vec![3], vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[
            vec![event((0, 0), vec![], vec![exact(1, 8, 8)])],
            vec![],
            vec![],
            vec![event((3, 0), vec![exact(1, 8, 8)], vec![])],
        ],
    );

    assert_eq!(analysis.only_clobber(), TestClobber::Definition((0, 0)));
}

#[test]
fn disjoint_loop_definition_does_not_hide_dominating_store() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1], vec![1, 2], vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[
            vec![event((0, 0), vec![], vec![exact(1, 8, 8)])],
            vec![
                event((1, 0), vec![], vec![exact(1, 64, 8)]),
                event((1, 1), vec![exact(1, 8, 8)], vec![]),
            ],
            vec![],
        ],
    );

    assert_eq!(analysis.only_clobber(), TestClobber::Definition((0, 0)));
    assert_eq!(analysis.graph.phi_count(), 1);
}

#[test]
fn aliasing_loop_definition_resolves_to_the_header_phi() {
    let cfg = ControlFlowGraph::analyze(vec![vec![1], vec![1, 2], vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[
            vec![event((0, 0), vec![], vec![exact(1, 8, 8)])],
            vec![event((1, 0), vec![], vec![exact(1, 8, 1)])],
            vec![],
        ],
    );

    let exit = analysis.points.block_exit(1).unwrap();
    let entry = analysis.points.block_entry(1).unwrap();
    let mut oracle = |definition: &Instruction, query: &MemoryEffect<Object>| {
        analysis.writes[definition]
            .iter()
            .copied()
            .any(|write| effects_may_alias(write, *query))
    };
    let classify = |clobber| match clobber {
        MemoryClobber::Indeterminate => TestClobber::Indeterminate,
        MemoryClobber::Access(access) => match analysis.graph.access(access).unwrap() {
            MemoryAccess::LiveOnEntry => TestClobber::LiveOnEntry,
            MemoryAccess::Definition { definition, .. } => TestClobber::Definition(*definition),
            MemoryAccess::Phi { block, .. } => TestClobber::Phi(block),
        },
    };
    let mut walker = ClobberWalker::new();
    assert_eq!(
        classify(
            walker
                .clobber(&analysis.graph, exit, &exact(1, 8, 8), &mut oracle)
                .unwrap()
        ),
        TestClobber::Definition((1, 0))
    );
    assert_eq!(
        classify(
            walker
                .clobber(&analysis.graph, entry, &exact(1, 8, 8), &mut oracle)
                .unwrap()
        ),
        TestClobber::Phi(1)
    );
}

#[test]
fn graph_size_is_independent_of_effect_range_length() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let analysis = analyze(
        &cfg,
        &[vec![
            event((0, 0), vec![], vec![exact(1, 1_000_000, 16 * 1024 * 1024)]),
            event((0, 1), vec![exact(1, 2_000_000, 8)], vec![]),
        ]],
    );

    assert_eq!(analysis.only_clobber(), TestClobber::Definition((0, 0)));
    assert_eq!(analysis.graph.definition_count(), 1);
    assert_eq!(analysis.graph.access_count(), 2);
}

#[test]
fn custom_alias_domain_needs_no_memory_effect_types() {
    let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
    let (graph, points) = build(
        &cfg,
        &[vec![
            MemoryAccessEvent {
                point: 0u8,
                definition: Some(10u8),
            },
            MemoryAccessEvent {
                point: 1u8,
                definition: None,
            },
        ]],
    )
    .unwrap();
    let mut oracle = |definition: &u8, query: &u8| definition % 10 == *query;
    let mut walker = ClobberWalker::new();
    let clobber = walker
        .clobber(&graph, points.event(1).unwrap().before, &0, &mut oracle)
        .unwrap();
    let MemoryClobber::Access(access) = clobber else {
        panic!("the custom oracle finds one definition")
    };
    assert!(matches!(
        graph.access(access),
        Some(MemoryAccess::Definition { definition: 10, .. })
    ));
}
