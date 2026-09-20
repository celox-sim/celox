//! Isolate MIR optimization from frontend parsing and circuit elaboration.

use std::hint::black_box;

use celox_backend_x86::native::{mir::*, mir_opt};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

fn chain(length: u32, observed: bool) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let mut block = MBlock::new(BlockId(0));
    let mut value = vregs.alloc();
    block.push(MInst::Load {
        dst: value,
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    });
    for _ in 0..length {
        let dst = vregs.alloc();
        block.push(MInst::AddImm {
            dst,
            src: value,
            imm: 1,
        });
        value = dst;
    }
    if observed {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: value,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Return);
    let spill_descs = vec![SpillDesc::transient(); vregs.count() as usize];
    let mut function = MFunction::new(vregs, spill_descs);
    function.blocks.push(block);
    function
}

fn compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("x86/cleanup");
    for observed in [false, true] {
        for length in [256, 1024, 4096] {
            let function = chain(length, observed);
            group.bench_with_input(
                BenchmarkId::new(if observed { "live_chain" } else { "dead_chain" }, length),
                &function,
                |b, function| {
                    b.iter_batched(
                        || function.clone(),
                        |mut function| {
                            mir_opt::post_regalloc_cleanup(&mut function);
                            black_box(function)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    group.finish();
}

fn live_through_blocks(block_count: u32, reverse_storage: bool) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let mut entry = MBlock::new(BlockId(0));
    let mut exit = MBlock::new(BlockId(block_count + 1));
    for index in 0..64 {
        let value = vregs.alloc();
        entry.push(MInst::Load {
            dst: value,
            base: BaseReg::SimState,
            offset: index * 8,
            size: OpSize::S64,
        });
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: (index + 64) * 8,
            src: value,
            size: OpSize::S64,
        });
    }
    entry.push(MInst::Jump { target: BlockId(1) });
    exit.push(MInst::Return);
    let spill_descs = vec![SpillDesc::transient(); vregs.count() as usize];
    let mut function = MFunction::new(vregs, spill_descs);
    function.blocks.push(entry);
    for index in 1..=block_count {
        let mut block = MBlock::new(BlockId(index));
        block.push(MInst::Jump {
            target: BlockId(index + 1),
        });
        function.blocks.push(block);
    }
    function.blocks.push(exit);
    if reverse_storage {
        // Entry remains first, but MIR storage need not follow CFG order.
        function.blocks[1..].reverse();
    }
    function
}

fn optimization(c: &mut Criterion) {
    let mut group = c.benchmark_group("x86/optimize");
    for reverse_storage in [false, true] {
        for block_count in [128, 512] {
            let function = live_through_blocks(block_count, reverse_storage);
            group.bench_with_input(
                BenchmarkId::new(
                    if reverse_storage {
                        "reverse_blocks"
                    } else {
                        "forward_blocks"
                    },
                    block_count,
                ),
                &function,
                |b, function| {
                    b.iter_batched(
                        || function.clone(),
                        |mut function| {
                            mir_opt::optimize(&mut function);
                            black_box(function)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, compilation, optimization);
criterion_main!(benches);
