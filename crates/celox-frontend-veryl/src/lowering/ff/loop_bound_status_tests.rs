use super::{FfParser, LoopBoundStatus};
use veryl_analyzer::ir::{ForBound, Op};

#[test]
fn allows_exclusive_upper_sentinel() {
    assert_eq!(
        FfParser::loop_bound_status(&ForBound::Const(255, true), 8, false),
        Some(LoopBoundStatus::FitsLoopType)
    );
    assert_eq!(
        FfParser::loop_bound_status(&ForBound::Const(256, true), 8, false),
        Some(LoopBoundStatus::ExclusiveUpperSentinel)
    );
    assert_eq!(
        FfParser::loop_bound_status(&ForBound::Const(257, true), 8, false),
        Some(LoopBoundStatus::OutOfRange)
    );
}

#[test]
fn bitwise_stall_checks_use_the_i32_loop_counter_width() {
    let Some(above_i32) = 1usize.checked_shl(32) else {
        return;
    };

    for (op, step, start, expected) in [
        (Op::BitXor, above_i32, Some(0), true),
        (Op::BitXor, above_i32 | 1, Some(0), false),
        (Op::BitOr, above_i32, Some(3), true),
        (Op::BitOr, above_i32 | 3, Some(3), true),
        (Op::BitOr, above_i32 | 4, Some(3), false),
        (Op::BitOr, above_i32 | 3, Some(above_i32 | 3), true),
    ] {
        assert_eq!(
            FfParser::step_can_stall(false, Some(op), step, start, 32),
            expected,
            "op={op:?}, step={step:#x}, start={start:?}",
        );
    }

    assert!(!FfParser::step_can_stall(
        false,
        Some(Op::BitXor),
        above_i32,
        Some(0),
        33,
    ));
}
