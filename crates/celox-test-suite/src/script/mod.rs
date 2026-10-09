//! Declarative test scripts.
//!
//! A script group is an S-expression file (see [`sexpr`]) with one
//! `(group NAME (category C))` header followed by `(case NAME ...)` forms.
//! Scripts keep each case's source, stimulus and expectations independent of
//! any host language: Celox and other Rust hosts run them with the
//! interpreter, and external simulators run them as a generated
//! SystemVerilog testbench, without a process protocol in between.
//!
//! # Statements
//!
//! | Form | Meaning |
//! | --- | --- |
//! | `(set SIG EXPR)` | Stage a write to a signal. |
//! | `(eval)` | Settle staged writes. Reads and ticks also settle them. |
//! | `(modify STMT...)` | Run the statements, then settle. |
//! | `(tick EVENT [COUNT])` | Pulse a clock or reset port COUNT times (default 1). |
//! | `(assert_eq SIG EXPR [MSG])` | The settled signal equals EXPR, unknown bits included. |
//! | `(assert EXPR [MSG])` | EXPR is non-zero. |
//! | `(let NAME EXPR)` / `(set! NAME EXPR)` | Bind / update a variable. |
//! | `(for NAME (range A B [STEP]) STMT...)` | Loop over A, A+STEP, ... before B. |
//! | `(for NAME (list E...) STMT...)` | Loop over the listed values. |
//! | `(if EXPR STMT [STMT])`, `(do STMT...)` | Conditionals and blocks. |
//! | `(expand NAME (ITEM...) STMT...)` | Repeat the statements with `{NAME}` replaced by each item's text (in names and messages); `(expand (A B) ((a1 b1) ...) ...)` binds several. |
//! | `(run_testbench)` | Run the design's native testbench to `$finish`. |
//! | `(expect_output TEXT)` | The design printed exactly TEXT with `$display`/`$write` since the start or the previous `expect_output`. |
//! | `(run_until TIME)` | Advance simulation time to TIME, running the design's processes, delays and clocks. TIME counts the design's time units and must not be in the past; a design that finishes first fails the case. |
//! | `(run_to_finish)` | Run until a process of the design calls `$finish`; a simulation that runs out of events first fails the case. Only `assert_eq`, `assert` and `expect_output` may follow, and read the final state. |
//!
//! A case with `run_until` or `run_to_finish` is timed: the design's own
//! `initial` and `always` processes run, and a write takes effect at the
//! current time, so a process waiting for the signal resumes when time next
//! advances. `tick` is not available in a timed case.
//!
//! A signal is `name` at the top or `inst.name`, `inst[2].name` below it.
//!
//! # Expressions
//!
//! Values are integers of unbounded width. A value read with `(get SIG)` is the
//! signal's unsigned bit pattern and may have unknown bits; arithmetic and
//! comparisons other than `==` / `!=` reject unknown bits, so an expectation
//! never borrows the simulator's width or X rules. Literals are decimal,
//! `0x`/`0o`/`0b` prefixed or sized (`8'hff`, `4'b10xz`).
//!
//! Operators: `+ - * / % **` (division truncates toward zero), `& | ^ ~`,
//! `<< >>` (`>>` floors), `== !=` (exact, unknown bits included),
//! `< <= > >=`, `! && ||`, `(? C A B)`, `min`, `max`, `(trunc W V)`,
//! `(sext W V)`, `(slice V HI LO)`, `(bit V I)`, `(cat W1 V1 W2 V2 ...)`,
//! `(rep N W V)`, `(fourstate P M)`, `(payload V)`, `(xmask V)`, `(known V)`,
//! `(popcount V)`, `(bitlen V)`.
//! Unknown bits use the backend encoding: X is payload 1 / mask 1, Z is
//! payload 0 / mask 1.
//!
//! An expected value must be non-negative: write `(trunc 8 -32)` for the
//! bit pattern of an 8-bit -32.

pub mod ast;
pub(crate) mod interp;
pub mod sexpr;
pub mod sv;

pub use ast::{ScriptCase, ScriptError};
