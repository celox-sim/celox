use celox_frontend_sdk::{BinaryOp, ModuleBuilder, ValueType};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let byte = ValueType::bits(8)?;
    let mut module = ModuleBuilder::new("NetAdder")?;
    let a = module.input("a", byte)?;
    let b = module.input("b", byte)?;
    let y = module.output("y", byte)?;
    let a = module.read(a)?;
    let b = module.read(b)?;
    let sum = module.binary(BinaryOp::Add, a, b, byte)?;
    let y = module.whole(y)?;
    module.assign(y, sum)?;
    println!("{}", module.finish().to_json()?);
    Ok(())
}
