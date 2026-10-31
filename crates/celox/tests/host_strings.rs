#![cfg(feature = "systemverilog")]
use celox::{SimBackend, Simulation, Simulator, SlotIo, SlotObserver};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
const SOURCE: &str =
    "module Top(input string text, output int size); always_comb size=text.len(); endmodule";

fn check_modify<B: SimBackend>(mut sim: Simulator<B>) {
    let text = sim.signal("text");
    let size = sim.signal("size");
    for bytes in [b"host".as_slice(), b"", b"\xff\xfe", b"longer string"] {
        sim.modify(|io| {
            io.set_string(text, bytes);
            assert_eq!(io.get_string(text), bytes);
        })
        .unwrap();
        assert_eq!(sim.get_as::<u32>(size), bytes.len() as u32);
    }
}

struct StringsInSlot {
    text: celox::SignalRef,
    fired: Arc<AtomicBool>,
}
impl<B: SimBackend> SlotObserver<B> for StringsInSlot {
    fn before_domains(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
    fn end_of_slot(&mut self, io: &mut SlotIo<'_, B>, _: u64) {
        if !self.fired.swap(true, Ordering::SeqCst) {
            assert_eq!(io.get_string(self.text), b"host");
            io.set_string(self.text, b"slot");
            assert_eq!(io.get_string(self.text), b"slot");
        }
    }
}
fn check_slot<B: SimBackend>(mut sim: Simulation<B>) {
    let text = sim.signal("text");
    sim.modify(|io| io.set_string(text, b"host")).unwrap();
    let fired = Arc::new(AtomicBool::new(false));
    sim.observe_slots(
        Box::new(StringsInSlot {
            text,
            fired: fired.clone(),
        }),
        &[],
    );
    sim.run_until(2000).unwrap();
    assert!(fired.load(Ordering::SeqCst));
    assert_eq!(sim.peek_string(text), b"slot");
}

#[test]
fn host_string_access_uses_each_backends_heap_and_slot_tracking() {
    let source = || vec![(SOURCE, Path::new("host_strings.sv"))];
    check_modify(
        Simulator::from_sv_sources(source(), "Top")
            .build_interpreter()
            .unwrap(),
    );
    check_modify(
        Simulator::from_sv_sources(source(), "Top")
            .build_native()
            .unwrap(),
    );
    check_modify(
        Simulator::from_sv_sources(source(), "Top")
            .build_tiered()
            .unwrap(),
    );
    #[cfg(feature = "cranelift")]
    check_modify(
        Simulator::from_sv_sources(source(), "Top")
            .build_cranelift()
            .unwrap(),
    );
    #[cfg(feature = "wasm-runtime")]
    check_modify(
        Simulator::from_sv_sources(source(), "Top")
            .build_wasm()
            .unwrap(),
    );

    let timed = "module Top(input string text, output int size); bit clk=0; always #1 clk=~clk; always_comb size=text.len(); endmodule";
    let source = || vec![(timed, Path::new("slot_strings.sv"))];
    check_slot(
        Simulation::from_sv_sources(source(), "Top")
            .build_interpreter()
            .unwrap(),
    );
    check_slot(
        Simulation::from_sv_sources(source(), "Top")
            .build_native()
            .unwrap(),
    );
    check_slot(
        Simulation::from_sv_sources(source(), "Top")
            .build_tiered()
            .unwrap(),
    );
    #[cfg(feature = "cranelift")]
    check_slot(
        Simulation::from_sv_sources(source(), "Top")
            .build_cranelift()
            .unwrap(),
    );
    #[cfg(feature = "wasm-runtime")]
    check_slot(
        Simulation::from_sv_sources(source(), "Top")
            .build_wasm()
            .unwrap(),
    );
}
