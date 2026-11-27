//! The activation counters of the compiled scheduler
//! (`SimulatorBuilder::activation_profile`) count what each slot ran and
//! leave the results alone.
#![cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]

use celox::{ActivationKind, ActivationProfile, Simulation};
use std::path::Path;

const DESIGN: &str = "`timescale 1ns/1ns
module Top;
  logic clk = 0;
  logic [7:0] cnt = 0;
  logic [7:0] dbl;
  always #5 clk = ~clk;
  always_ff @(posedge clk) cnt <= cnt + 8'd1;
  assign dbl = cnt * 8'd2;
endmodule
";

fn simulation(profile: bool) -> Simulation<impl celox::SimBackend> {
    Simulation::from_sv_sources(vec![(DESIGN, Path::new("profile.sv"))], "Top")
        .activation_profile(profile)
        .build_native()
        .unwrap()
}

fn outputs(sim: &mut Simulation<impl celox::SimBackend>) -> (u64, u64) {
    let cnt = sim.signal("cnt");
    let dbl = sim.signal("dbl");
    (
        sim.get(cnt).try_into().unwrap(),
        sim.get(dbl).try_into().unwrap(),
    )
}

fn count(profile: &ActivationProfile, kind: ActivationKind, measure: &str) -> u64 {
    profile
        .counters
        .iter()
        .filter(|counter| counter.site.kind == kind && counter.site.measure == measure)
        .map(|counter| counter.count)
        .sum()
}

#[test]
fn counts_slots_domains_and_quiet_judgments() {
    let mut plain = simulation(false);
    assert!(plain.activation_profile().is_none());
    plain.run_until(100).unwrap();

    let mut sim = simulation(true);
    sim.run_until(100).unwrap();
    assert_eq!(outputs(&mut sim), outputs(&mut plain));
    assert_eq!(outputs(&mut sim).0, 10, "posedges at 5, 15, ..., 95");

    let profile = sim
        .activation_profile()
        .expect("built with activation_profile");
    let text = format!("{profile:#?}");
    let stats = sim.scheduler_stats();
    assert!(stats.compiled_slots > 0, "{stats:?}");
    let slots = count(&profile, ActivationKind::Slot, "runs");
    assert!(slots > 0 && slots <= stats.compiled_slots, "{text}");
    // The clock kernel toggles the clock in every compiled slot after the
    // first; each posedge runs the domain once.
    let domains: u64 = ["eval_apply", "eval_only", "fused"]
        .iter()
        .map(|measure| count(&profile, ActivationKind::Domain, measure))
        .sum();
    let edges = count(&profile, ActivationKind::Kernel, "edges");
    assert_eq!(domains, edges, "{text}");
    assert!(domains > 0 && domains <= 10, "{text}");
    // A negedge slot judges and finds no domain.
    let judged = count(&profile, ActivationKind::DomainJudge, "judged");
    let quiet = count(&profile, ActivationKind::DomainJudge, "quiet");
    assert!(judged >= domains + quiet - quiet.min(judged), "{text}");
    assert!(quiet > 0 && quiet < judged, "{text}");
    assert!(
        count(&profile, ActivationKind::Kernel, "runs") >= slots - 1,
        "{text}"
    );
}

#[test]
fn clean_settle_requests_enter_only_when_observed() {
    struct Updates;
    impl<B: celox::SimBackend> celox::SlotObserver<B> for Updates {
        fn before_domains(&mut self, _: &mut celox::SlotIo<'_, B>, _: u64) {}
        fn end_of_slot(&mut self, _: &mut celox::SlotIo<'_, B>, _: u64) {}
        fn observes_updates(&self) -> bool {
            true
        }
    }

    let source = DESIGN.replace(
        "endmodule",
        "assert property (@(posedge clk) cnt < 128); endmodule",
    );
    for watching in [false, true] {
        let mut sim =
            Simulation::from_sv_sources(vec![(source.as_str(), Path::new("settle.sv"))], "Top")
                .observable_updates(true)
                .activation_profile(true)
                .build_native()
                .unwrap();
        if watching {
            sim.observe_slots(Box::new(Updates), &[]);
        }
        sim.run_until(100).unwrap();
        assert_eq!(outputs(&mut sim), (10, 20));
        let profile = sim.activation_profile().unwrap();
        let requests = count(&profile, ActivationKind::SettleCall, "requests");
        let entered = count(&profile, ActivationKind::Settle, "entered");
        let dirty = count(&profile, ActivationKind::Settle, "dirty");
        assert!(requests > dirty && dirty > 0);
        assert_eq!(entered, if watching { requests } else { dirty });
    }
}

#[test]
fn a_profiled_native_image_counts_after_it_is_reloaded() {
    let mut built = simulation(true);
    built.run_until(100).unwrap();
    let image = Simulation::from_sv_sources(vec![(DESIGN, Path::new("profile.sv"))], "Top")
        .activation_profile(true)
        .compile_native()
        .unwrap()
        .into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let mut loaded = Simulation::from_sv_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    loaded.run_until(100).unwrap();
    assert_eq!(outputs(&mut loaded), outputs(&mut built));
    assert_eq!(loaded.activation_profile(), built.activation_profile());
}
