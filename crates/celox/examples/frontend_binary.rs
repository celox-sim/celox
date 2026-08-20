mod my_frontend {
    use celox::Simulator;
    use celox_frontend_sdk::{BinaryOp, ModuleBuilder, ValueType};

    pub struct MyArtifact {
        pub module_name: String,
        pub width: usize,
    }

    pub trait MyFrontendSimulatorExt: Sized {
        fn from_my_artifact(artifact: MyArtifact) -> Result<Self, Box<dyn std::error::Error>>;
    }

    impl MyFrontendSimulatorExt for Simulator {
        fn from_my_artifact(artifact: MyArtifact) -> Result<Self, Box<dyn std::error::Error>> {
            let value_type = ValueType::bits(artifact.width)?;
            let mut module = ModuleBuilder::new(artifact.module_name)?;
            let a = module.input("a", value_type)?;
            let b = module.input("b", value_type)?;
            let y = module.output("y", value_type)?;
            let a_expr = module.read(a)?;
            let b_expr = module.read(b)?;
            let sum = module.binary(BinaryOp::Add, a_expr, b_expr, value_type)?;
            let y_target = module.whole(y)?;
            module.assign(y_target, sum)?;

            Ok(Simulator::from_frontend(module.finish()).build()?)
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    use my_frontend::MyFrontendSimulatorExt as _;

    let artifact = my_frontend::MyArtifact {
        module_name: "NetAdder".into(),
        width: 8,
    };
    let mut sim = celox::Simulator::from_my_artifact(artifact)?;
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 23u8);
    })?;

    let result = sim.get(y);
    println!("10 + 23 = {result}");
    assert_eq!(result, 33u8.into());
    Ok(())
}
