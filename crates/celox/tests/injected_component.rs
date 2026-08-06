#![cfg(feature = "host-runtime")]

use std::sync::{Arc, Mutex};

use celox::{
    InjectedCall, InjectedComponentHandler, InjectedComponents, InjectedHook, InjectedNamedValue,
    InjectedResult, InjectedValue, Simulator, TestResult,
};

struct Model {
    phases: Arc<Mutex<Vec<String>>>,
    fired_clocks: Arc<Mutex<Vec<Option<String>>>>,
}

impl InjectedComponentHandler for Model {
    fn call(&self, call: InjectedCall) -> Result<InjectedResult, String> {
        let phase = match call.hook {
            InjectedHook::Create => "create",
            InjectedHook::Init => "init",
            InjectedHook::Clock => "clock",
            InjectedHook::Finish => "finish",
            InjectedHook::Reset => "reset",
            InjectedHook::Method { .. } => "method",
        };
        self.phases.lock().unwrap().push(phase.into());
        self.fired_clocks
            .lock()
            .unwrap()
            .push(call.fired_clock.clone());
        let output = match call.hook {
            InjectedHook::Init => Some(0),
            InjectedHook::Clock => {
                let input = call
                    .inputs
                    .iter()
                    .find(|value| value.name == "d")
                    .and_then(|value| match &value.value {
                        InjectedValue::Bits { words, .. } => words.first().copied(),
                        _ => None,
                    })
                    .unwrap();
                Some(input + 3)
            }
            _ => None,
        };
        Ok(InjectedResult {
            outputs: output
                .map(|value| InjectedNamedValue {
                    name: "q".into(),
                    value: InjectedValue::Bits {
                        words: vec![value],
                        mask_xz: vec![0],
                        width: 8,
                    },
                })
                .into_iter()
                .collect(),
            ..Default::default()
        })
    }
}

#[test]
fn injected_clocked_component_uses_component_scheduling() {
    let source = r#"
        #[test(InjectedComponentTb)]
        module InjectedComponentTb {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst model: $comp::ts_model (clk, d, q);

            initial {
                $assert(q == 0, "on_init output");
                d = 8'h20;
                clk.next();
                $assert(q == 8'h23, "injected component output");
                $finish();
            }
        }
    "#;
    let phases = Arc::new(Mutex::new(Vec::new()));
    let fired_clocks = Arc::new(Mutex::new(Vec::new()));
    let mut components = InjectedComponents::new();
    components
        .insert(
            "ts_model",
            r#"{
                "kind":"clocked",
                "ports":[
                    {"name":"clk","dir":"input","role":"clock"},
                    {"name":"d","dir":"input"},
                    {"name":"q","dir":"output"}
                ]
            }"#,
            Arc::new(Model {
                phases: phases.clone(),
                fired_clocks: fired_clocks.clone(),
            }),
        )
        .unwrap();

    let result = Simulator::builder(source, "InjectedComponentTb")
        .with_injected_components(components)
        .run_test()
        .unwrap();
    assert_eq!(result, TestResult::Pass, "{result:?}");
    assert_eq!(
        *phases.lock().unwrap(),
        ["create", "init", "clock", "finish"]
    );
    assert_eq!(
        *fired_clocks.lock().unwrap(),
        [None, None, Some("clk".to_string()), None]
    );
}
