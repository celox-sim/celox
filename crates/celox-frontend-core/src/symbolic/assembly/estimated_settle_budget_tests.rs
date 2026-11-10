use super::*;

#[test]
fn estimated_guard_count_stays_bounded_as_the_program_grows() {
    for cost in [
        20_000,
        100_000,
        1_000_000,
        3_000_000,
        100_000_000,
        usize::MAX,
    ] {
        let target = estimated_settle_partition_cost(cost);
        assert!(target >= SETTLE_PARTITION_COST);
        assert!(cost.div_ceil(target) <= TARGET_ESTIMATED_SETTLE_PARTITIONS);
    }
}
