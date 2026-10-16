//! Keep this capture in its own test binary so its global subscriber cannot
//! collect events from unrelated runtime tests. A thread-local subscriber can
//! lose callsite interest when another thread first uses LanePool without it
//! (https://github.com/celox-sim/celox/issues/1374).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use celox_runtime::parallel::{
    LanePool, LaneSchedule, LaneTaskFailure, LaneTaskRunner, LaneTaskSpec,
};

struct Capture(Arc<Mutex<Vec<BTreeMap<String, String>>>>);
struct Fields(BTreeMap<String, String>);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}

impl tracing::Subscriber for Capture {
    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(tracing::metadata::LevelFilter::TRACE)
    }
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "celox::parallel"
    }
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
}

struct Runner {
    fail: Option<usize>,
}

impl LaneTaskRunner for Runner {
    fn run_task(&self, task: usize) -> i64 {
        if self.fail == Some(task) { 7 } else { 0 }
    }
}

#[test]
fn traced_failure_and_recovery_record_skips_and_real_waits() {
    let events = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::set_global_default(Capture(events.clone())).unwrap();

    // First use the runtime's callsites on a different thread from the one
    // that installed the subscriber, as happens with concurrent unit tests.
    std::thread::spawn(|| {
        // lane 0: t0, t2; lane 1: t1 (waits for t0); lane 2: t3 (waits for t1, t2)
        let schedule = LaneSchedule::new(
            3,
            vec![
                LaneTaskSpec {
                    lane: 0,
                    waits: vec![],
                },
                LaneTaskSpec {
                    lane: 1,
                    waits: vec![(0, 1)],
                },
                LaneTaskSpec {
                    lane: 0,
                    waits: vec![],
                },
                LaneTaskSpec {
                    lane: 2,
                    waits: vec![(1, 1), (0, 2)],
                },
            ],
        )
        .unwrap();
        let mut pool = LanePool::new(3).unwrap();
        assert_eq!(
            pool.run(&schedule, &Runner { fail: Some(0) }),
            Err(LaneTaskFailure { task: 0, code: 7 })
        );
        pool.run(&schedule, &Runner { fail: None }).unwrap();
    })
    .join()
    .unwrap();

    let events = events.lock().unwrap();
    let rows = |name: &str| {
        let name = format!("{name:?}");
        events.iter().filter(move |e| e["event"] == name)
    };
    assert_eq!(rows("dispatch").count(), 2, "{events:?}");
    let runs = rows("task_run").collect::<Vec<_>>();
    assert_eq!(runs.len(), 8);
    assert!(runs.iter().any(|e| e["skipped"] == "true"));
    for event in runs.iter().filter(|e| e["epoch"] == "2") {
        assert_eq!(event["skipped"], "false");
        assert_eq!(event["code"], "0");
        assert!(
            event["end_ns"].parse::<u64>().unwrap() >= event["start_ns"].parse::<u64>().unwrap()
        );
    }
    assert_eq!(rows("task_wait").filter(|e| e["epoch"] == "2").count(), 3);
}
