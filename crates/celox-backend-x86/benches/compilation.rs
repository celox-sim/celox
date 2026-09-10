//! Isolate backend cleanup from frontend parsing and circuit elaboration.

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

criterion_group!(benches, compilation);
criterion_main!(benches);
