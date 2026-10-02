//! Measure the full-domain 32 x 6-bit update found in Heliodor's FP free list.
#[cfg(target_arch = "x86_64")]
#[path = "../tests/support/packed_index_fixture.rs"]
mod fixture;
#[cfg(target_arch = "x86_64")]
use criterion::{Criterion, criterion_group, criterion_main};
#[cfg(target_arch = "x86_64")]
use std::hint::black_box;

#[cfg(target_arch = "x86_64")]
fn packed_index_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("x86/packed-index-update");
    for recover in [false, true] {
        let mut compiled = fixture::compile(recover);
        let mut input = 0u8;
        group.bench_function(if recover { "direct" } else { "scan" }, |b| {
            b.iter(|| {
                input = input.wrapping_add(1);
                let bytes = compiled.bytes();
                bytes[0] = input & 31;
                bytes[8] = input & 63;
                assert_eq!(compiled.execute(), 0);
                black_box(&compiled.bytes()[56..80]);
            });
        });
    }
    group.finish();
}
#[cfg(target_arch = "x86_64")]
criterion_group!(benches, packed_index_update);
#[cfg(target_arch = "x86_64")]
criterion_main!(benches);

#[cfg(not(target_arch = "x86_64"))]
fn main() {}
