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

    // The key assertion: building a simulator with two different packages
    // instantiating the same generic module must succeed. Before the ModuleId
    // refactor, this would fail because both instantiations shared a single
    // module identity.
    let _sim = Simulator::builder(code, "Top").build().unwrap();
}
