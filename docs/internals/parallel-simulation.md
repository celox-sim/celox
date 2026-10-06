# Parallel Execution

`SimulatorBuilder::threads(n)` compiles lane-partitioned alternatives of the
two hot phases of a simulation step: the combinational settle and the
sequential update of each clock or reset event. This page describes how the
partition is built, why concurrent lanes never corrupt each other's state,
and how the runtime executes and selects them.

## Overview

```text
LogicPath graph (MemorySSA) ──► lane assignment ──► per-task lowering ──┐
FF trigger groups ─────────────► FF parts ─────────► part placement ────┤
                                                                        ▼
                                         ParallelSirProgram (lane units)
                                                                        │
                       per-unit optimization (same pipelines as split phases)
                                                                        │
                        lane writers ──► lane-segmented state layout ◄──┤
                                                                        │
                final effects ──► happens-before ──► lane tasks ◄───────┘
                                                                        │
                one function per task, one native arena per lane        │
                                                                        ▼
                     LanePool execution + measured kernel selection
```

The sequential kernels are compiled unchanged. The partitioned kernels are
alternatives: every unit list is a valid sequential order, so running a
partitioned kernel's units one after another is equivalent to its sequential
kernel.

## Partitioning the combinational settle

The SLT scheduler already builds a MemorySSA dependency graph of LogicPaths,
condenses its SCCs, and orders the result. `scheduler::sort_lanes` reuses
exactly that graph and order:

1. Scheduled work items (paths, loop SCCs, and guarded regions) are joined
   into atomic items, each lowered through one SIR builder:
   - a comb observer whose arguments are materialized by a later write is
     joined with that write;
   - work that shares an expensive SLT value is joined, because a separate
     builder would compute the value again. Shared values below a small cost
     (a load, a slice) are recomputed instead, so a common input does not
     serialize its readers.

   Joined work that depends on itself through other work also absorbs that
   work, so the condensed graph stays acyclic.
2. Each item's cost counts every SLT value it reaches once, including the
   cheap values it shares, since its builder recomputes them. The
   single-lane cost counts every value once.
3. Two candidate partitions are built with a list scheduler that places the
   ready item with the longest remaining path first, so work that another
   lane waits for runs early:
   - *items*: every item goes to the lane where it can start earliest. A
     dependency on another lane costs a synchronization delay, and an item
     stays in its latest producer's lane (or the previous item's) while that
     lane is at most a small slack behind, so runs of neighbouring items
     keep their cache lines in one core;
   - *instance subtrees*: walking the instance tree from the top, every
     subtree whose cost fits one lane's share is kept whole, otherwise the
     instance's own logic is one cluster and its children are considered.
     Clusters are placed largest first on the least loaded lane. Logic of
     one subtree mostly communicates inside it, so this partition needs few
     waits; on an SMP SoC each core lands in its own lane.
4. Items that write overlapping bytes of one state object share a lane.
   Writers of disjoint bytes, such as separate array elements, may be split.
   Runtime events and stores to event signals stay in lane 0 because they
   update shared buffers.
5. Each lane's items are cut into tasks: maximal runs that need no new wait
   on another lane. Each candidate is then charged for the task boundaries
   and waits it actually creates, and the cheaper one is kept. Every task is
   lowered through its own SIR builder.

Costs count 64-bit words per value, capped at a few dozen words: wider
values are arrays and memories, which lowering only accesses element by
element.

If the estimated makespan plus one synchronization does not beat the
single-lane cost by a margin, the phase keeps only its sequential kernel.

## Partitioning sequential updates

FF actions sample pre-edge state and stage their own targets, so independent
actions can be evaluated concurrently and published afterwards. Every
instance's trigger group contributes evaluate/apply units. For partitioned
builds, Veryl also lowers each trigger group as several contiguous *parts* of
its `always_ff` declarations, so even a single large module provides
independent work. Parts writing overlapping bytes are grouped, and the groups
are placed with a longest-processing-time heuristic. A group prefers the lane
that accesses its state most in the partitioned combinational settle, while
that lane stays within a balanced share, so an instance's registers and the
logic reading them stay in one core's cache.

## Physical layout

Concurrent tasks never share native scratch storage or overlapping write
envelopes, but generated code may widen a partial store into a
read-modify-write of neighbouring bytes. The state layout therefore follows
the lanes:

- homes written by one lane are packed into that lane's segment;
- homes written by several lanes are isolated in their own segment;
- every segment starts on a 128-byte boundary at least 16 bytes after the
  previous one, so a widened access stays inside storage of the same lane;
- sparse write metadata is grouped by lane in the same way.

Without partitioned kernels the layout is unchanged.

## Happens-before from final effects

Optimization and identity aliasing run before code generation, so the
concurrency plan is computed from the final SIR and the finished layout
(`celox_sir_opt::parallel`):

- reads are exact physical byte ranges, so aliased objects compare by storage;
  element-strided arrays are resolved per element slot;
- a write records its exact bytes for read/write ordering and, for
  write/write ordering, every byte its code generator may store
  (`CodegenFootprint`). Native code writes a static field in parts of at most
  eight bytes and rounds the last part up to 1, 2, 4, or 8 bytes. Cranelift
  rounds a field within one word written from a narrow register the same
  way, and otherwise writes whole 8-byte words. Dynamically addressed writes
  cover their whole home and a few bytes past it;
- the runtime-event ring, comb-capture flags, and trigger bytes are
  serialized as whole resources; waveform notification bytes need no order,
  since generated code only ever sets them to one;
- units that store and commit sparse state never share a task, because one
  native function containing both commits every active sparse object.

Each task records only non-redundant waits as `(lane, completed task count)`
pairs; a wait implies everything its producer task waited for.

The native pipeline rewrites each task's merged SIR once more before
instruction selection: it coalesces stores and redirects staged writes, which
can change the bytes a task writes. The native backend therefore takes the
footprint of every task's final SIR and recomputes the waits of the fixed
task list before it packs the kernel.

## Code generation

Every task becomes one function with the ordinary `fn(state) -> status` ABI.
Native backends place spill and scratch storage after the simulation state;
all functions of one lane share an arena, and every lane's arena starts after
the arenas of lower lanes. Cranelift spills to the machine stack and needs no
arena. Native images record the task table and bump the container version.

## Runtime

`celox_runtime::parallel::LanePool` runs lane 0 on the calling thread and the
other lanes on persistent workers. Every lane publishes its completed task
count, and finally a done marker, in its own cache line with release stores;
consumers and the dispatching thread acquire them, so completion never
contends on a shared counter. Idle workers spin briefly and then park. After a
task fails, pending tasks are skipped and the lowest-indexed failure is
reported.

A `KernelSelector` measures each partitioned kernel against its sequential
version in alternating blocks of calls, discarding the first calls of each
block, and keeps the faster one. Combined comb-and-event calls also compete
with the fused sequential function, which keeps the native tick loop when it
wins. A decision is measured again after an interval that doubles with every
check, so a choice made while the host was busy does not stay wrong.
