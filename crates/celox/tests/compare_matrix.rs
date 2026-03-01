use celox::Simulator;

/// Tests that two different packages can instantiate the same generic module,
/// each getting a unique ModuleId. This validates the ModuleId refactor that
/// enables multiple concrete instantiations of a single generic module.
#[test]
fn test_generic_module_instantiation() {
    let code = r#"
proto package DataType {
    type data;
}

module GenericPass::<E: DataType> (
    i: input  E::data,
    o: output E::data,
) {
    assign o = i;
}

package Byte for DataType {
    type data = logic<8>;
}

package Word for DataType {
    type data = logic<16>;
}

module BytePass (
    i: input  logic<8>,
    o: output logic<8>,
) {
    inst inner: GenericPass::<Byte> (i, o);
}

module WordPass (
    i: input  logic<16>,
    o: output logic<16>,
) {
    inst inner: GenericPass::<Word> (i, o);
}

module Top (
    a: input  logic<8>,
    b: output logic<8>,
    c: input  logic<16>,
    d: output logic<16>,
) {
    inst bp: BytePass (i: a, o: b);
    inst wp: WordPass (i: c, o: d);
}
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let d = sim.signal("d");

    // Verify 8-bit passthrough via GenericPass::<Byte>
    sim.modify(|io| {
        io.set(a, 0xABu8);
        io.set(c, 0x1234u16);
    })
    .unwrap();
    assert_eq!(sim.get(b), 0xABu8.into());
    assert_eq!(sim.get(d), 0x1234u16.into());

    // Verify with different values
    sim.modify(|io| {
        io.set(a, 0xFFu8);
        io.set(c, 0xFFFFu16);
    })
    .unwrap();
    assert_eq!(sim.get(b), 0xFFu8.into());
    assert_eq!(sim.get(d), 0xFFFFu16.into());
}
