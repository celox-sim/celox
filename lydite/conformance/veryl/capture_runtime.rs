//! Local-only trace capture. Backend::read never yields a hardware value.
use crate::{
    Backend, BigUint, CompilationRejected, Design, Expectation, Result, Scalar, SignalPath,
    TestCase,
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
};
#[derive(Default)]
struct Context {
    actions: Vec<Value>,
    designs: Vec<Value>,
    unsupported: Vec<Value>,
}
thread_local! { static CONTEXT: RefCell<Context> = RefCell::new(Context::default()); }
fn path(signal: &SignalPath) -> Value {
    json!({"signal": signal.name, "instances": signal.instances.iter().map(|x| json!({"name":x.name,"index":x.index})).collect::<Vec<_>>()})
}
fn action(mut value: Value, signal: Option<&SignalPath>) -> usize {
    if let Some(signal) = signal {
        value
            .as_object_mut()
            .unwrap()
            .extend(path(signal).as_object().unwrap().clone());
    }
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let i = c.actions.len();
        c.actions.push(value);
        i
    })
}
fn unsupported(code: &str, detail: Value) {
    CONTEXT.with(|c| {
        c.borrow_mut()
            .unsupported
            .push(json!({"code":code,"detail":detail}))
    });
}
struct Recorder;
impl Backend for Recorder {
    fn write(&mut self, signal: &SignalPath, payload: BigUint, mask: BigUint) -> Result<()> {
        action(
            json!({"action":"write","payload":payload.to_string(),"mask":mask.to_string()}),
            Some(signal),
        );
        Ok(())
    }
    fn read(&mut self, signal: &SignalPath) -> Result<(BigUint, BigUint)> {
        unsupported("uncaptured_actual_read", path(signal));
        Err("an actual hardware read escaped a supported assertion; no fabricated value is available".into())
    }
    fn eval_comb(&mut self) -> Result<()> {
        action(json!({"action":"eval_comb"}), None);
        Ok(())
    }
    fn tick(&mut self, event: &str) -> Result<()> {
        action(json!({"action":"tick","event":event}), None);
        Ok(())
    }
}
pub fn begin(
    signal: SignalPath,
    kind: &str,
    comparison: &str,
    file: &str,
    line: u32,
    column: u32,
    expression: &str,
    sample: (&str, u32, u32),
    actual_form: &str,
) -> usize {
    action(
        json!({"action":"read", "assertion":{
            "comparison":comparison,"read_kind":kind,"actual_form":actual_form,"sample_location":{"file":sample.0,"line":sample.1,"column":sample.2},"location":{"file":file,"line":line,"column":column},"expression":expression,
            "projection":if kind=="get_four_state" {"payload_and_mask"} else if kind=="get_as" {"scalar_low_bits"} else {"payload"}
        }}),
        Some(&signal),
    )
}
fn finish(
    index: usize,
    payload: BigUint,
    mask: Option<BigUint>,
    width: Option<u32>,
    ty: Option<&str>,
) {
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let a = &mut c.actions[index];
        a["payload"] = json!(payload.to_string());
        // Compatibility field only: get() never constrains the mask.
        a["mask"] = json!(
            mask.as_ref()
                .map_or_else(|| "0".to_string(), ToString::to_string)
        );
        a["assertion"]["mask_constraint"] = json!(mask.as_ref().map(ToString::to_string));
        if let Some(width) = width {
            a["assertion"]["scalar_width"] = json!(width);
        }
        if let Some(ty) = ty {
            a["assertion"]["scalar_type"] = json!(ty);
        }
    });
}
pub fn finish_get(index: usize, expected: BigUint) {
    finish(index, expected, None, None, None);
}
pub fn finish_four_state(index: usize, expected: (BigUint, BigUint)) {
    finish(index, expected.0, Some(expected.1), None, None);
}
pub fn finish_scalar<T: Scalar>(index: usize, expected: T) {
    let ty = std::any::type_name::<T>();
    finish(
        index,
        expected.to_bits(),
        None,
        Some(if ty == "bool" {
            1
        } else {
            (size_of::<T>() * 8) as u32
        }),
        Some(ty),
    );
}
pub fn run_case(case: &TestCase) -> Value {
    CONTEXT.with(|c| *c.borrow_mut() = Context::default());
    let result = catch_unwind(AssertUnwindSafe(|| {
        case.run(&mut |design: &Design| {
        CONTEXT.with(|c| { let mut c = c.borrow_mut(); c.designs.push(json!({"top":design.top,"four_state":design.four_state,
            "sources":design.sources.iter().map(|s| json!({"path":s.path,"text":s.text})).collect::<Vec<_>>()}));
            if c.designs.len()>1 { c.unsupported.push(json!({"code":"multiple_designs","detail":"one design per trace is supported"})); }
        });
        if case.expectation == Expectation::CompilationError {
            // This records/classifies the rejection fixture, not an HDL compile result.
            return Err(CompilationRejected("capture-only: compilation not attempted".into()).into());
        }
        Ok(Box::new(Recorder) as Box<dyn Backend>)
    })
    }));
    CONTEXT.with(|c| {
        let mut c = c.borrow_mut();
        let panic = result.err().map(|p| p.downcast_ref::<String>().cloned().or_else(||p.downcast_ref::<&str>().map(|s|s.to_string())).unwrap_or_else(||"non-string panic".into()));
        if let Some(panic) = &panic { if c.unsupported.is_empty() { c.unsupported.push(json!({"code":"rust_execution_panic","detail":panic})); } }
        if c.actions.iter().any(|x| x["action"]=="read" && x.get("payload").is_none()) {
            c.unsupported.push(json!({"code":"incomplete_assertion_capture","detail":"expected expression did not complete"}));
        }
        let assertions = c.actions.iter().filter(|x| x["action"]=="read").count();
        let rejection = case.expectation == Expectation::CompilationError;
        if !rejection && assertions==0 && c.unsupported.is_empty() { c.unsupported.push(json!({"code":"no_output_assertions","detail":"stimulus-only smoke test is not an output specification"})); }
        let status = if !c.unsupported.is_empty() {"unsupported"} else if rejection {"compile_rejection_fixture"} else {"extracted"};
        let mut row = json!({"case":case.name,"category":format!("{:?}",case.category),"status":status,"assertion_count":assertions,
            "design":c.designs.first(),"reasons":c.unsupported,"panic":panic});
        if status=="extracted" { row["actions"]=json!(c.actions); }
        else { row["partial_actions"]=json!(c.actions); }
        row
    })
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
