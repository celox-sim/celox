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
