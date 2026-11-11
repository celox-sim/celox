use super::*;
use crate::ir::{
    BasicBlock, BlockId, RegisterId, RegisterType, SIRInstruction, SIRTerminator, SIRValue,
};

#[test]
fn replacing_code_installs_the_new_callee_table() {
    let image = crate::Simulator::builder(
        "module Top(a: input bit<8>, b: input bit<8>, y: output bit<8>) { assign y = a + b; }",
        "Top",
    )
    .compile_native()
    .unwrap()
    .into_program_image();
    // SAFETY: both images were produced by the compiler immediately above.
    let original = Arc::new(unsafe { SharedNativeCode::from_image(image.clone()).unwrap() });
    // SAFETY: the replacement is another load of the same compiler output.
    let replacement = Arc::new(unsafe { SharedNativeCode::from_image(image).unwrap() });
    let old_table = original.extern_functions.as_ptr() as usize;
    let new_table = replacement.extern_functions.as_ptr() as usize;
    assert_ne!(old_table, new_table);
    // Retain the old image so allocator reuse cannot hide a stale binding.
    let mut backend = NativeBackend::from_shared(original.clone());
    backend.replace_shared_code(replacement);
    let offset = celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET;
    // SAFETY: the fixed header slot lies within the backend's live image.
    let installed = unsafe {
        backend
            .memory
            .as_ptr()
            .cast::<u8>()
            .add(offset)
            .cast::<u64>()
            .read_unaligned()
    };
    assert_eq!(
        installed, new_table as u64,
        "generated code must use its own helper and kernel table"
    );
}

/// A unit is split at each of its stores, except one that makes a
/// call, which no part could repeat or leave out.
#[test]
fn units_with_calls_are_not_split_at_their_stores() {
    let addr = |id| {
        RegionedAbsoluteAddr::from_absolute_addr(
            celox_design::STABLE_REGION,
            crate::ir::AbsoluteAddr {
                instance_id: celox_design::InstanceId(0),
                var_id: celox_design::StateObjectId(id),
            },
        )
    };
    let store = |id, value| {
        SIRInstruction::Store(
            addr(id),
            celox_sir::SIROffset::Static(0),
            8,
            RegisterId(value),
            Vec::new(),
            Vec::new(),
        )
    };
    let unit = |instructions| ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(
            BlockId(0),
            celox_sir::BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: (0..2)
            .map(|id| {
                (
                    RegisterId(id),
                    celox_sir::RegisterType::Bit {
                        width: 8,
                        signed: false,
                    },
                )
            })
            .collect(),
    };
    let stores = vec![
        SIRInstruction::Imm(RegisterId(0), celox_sir::SIRValue::new(1u8)),
        store(1, 0),
        store(2, 0),
    ];
    assert_eq!(split_comb_execution_unit(&unit(stores.clone())).len(), 2);
    let mut calling = stores;
    calling.push(SIRInstruction::Call {
        function: celox_sir::FunctionId(0),
        frame: RegisterId(1),
        args: Vec::new(),
        outputs: Vec::new(),
        dst: None,
        effects: Default::default(),
    });
    let calling = unit(calling);
    assert_eq!(split_comb_execution_unit(&calling), vec![calling]);
}

#[test]
fn replacing_images_does_not_accumulate_alignment_padding() {
    let source =
        "module Top(a: input bit<8>, b: input bit<8>, y: output bit<8>) { assign y = a + b; }";
    let mut image = crate::Simulator::builder(source, "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let original_code = image.code.to_vec();
    assert!(original_code.len() < 16_383);
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("runtime");
    std::fs::write(&runtime, b"precompiled runtime").unwrap();
    let mut sizes = Vec::new();
    // Crossing a Postcard length-prefix boundary changes the required
    // padding. Trailing zeros test framing only and are never executed.
    for length in [16_383, 16_384, 16_383, 16_384] {
        let mut code = original_code.clone();
        code.resize(length, 0);
        image.code = code.into();
        image.write_attached_runtime(&runtime, &runtime).unwrap();
        let bytes = std::fs::read(&runtime).unwrap();
        let found = NativeProgramImage::discover_appended(&bytes)
            .unwrap()
            .unwrap();
        assert_eq!(found.image.code_image().len(), length);
        assert!(bytes.starts_with(b"precompiled runtime"));
        sizes.push(bytes.len());
    }
    assert_eq!(sizes[0], sizes[2]);
    assert_eq!(sizes[1], sizes[3]);
    assert_eq!(sizes[1], sizes[0] + 1);
}

#[test]
fn postcard_code_decoder_preserves_legacy_payloads() {
    let source = "module Top(clk: input clock, d: input bit<8>, q: output bit<8>) { always_ff (clk) { q = d; } }";
    let mut image = crate::Simulator::builder(source, "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    // These byte patterns exercise the wire decoder only; no synthetic
    // machine code is validated, mapped executable, or invoked.
    for length in [0, 1, 127, 128, 255, 256, 1024] {
        image.code = (0..length)
            .map(|index| index as u8)
            .collect::<Vec<_>>()
            .into();
        let payload = postcard::to_allocvec(&image).unwrap();
        let legacy: NativeProgramImage = postcard::from_bytes(&payload).unwrap();
        let decoded = NativeProgramImage::from_postcard_bytes(&payload).unwrap();
        assert_eq!(&*decoded.code, &*image.code);
        assert_eq!(
            postcard::to_allocvec(&decoded).unwrap(),
            postcard::to_allocvec(&legacy).unwrap(),
        );
        for end in [0, 1, payload.len() - 1] {
            assert!(postcard::from_bytes::<NativeProgramImage>(&payload[..end]).is_err());
            assert!(NativeProgramImage::from_postcard_bytes(&payload[..end]).is_err());
        }
    }
}

#[test]
fn image_without_clock_alias_exports_uses_full_settle() {
    let source = r#"
            module Child (clk: input clock, d: input bit<8>, q: output bit<8>) {
                always_ff (clk) { q = d; }
            }
            module Top (clk: input clock, d: input bit<8>, q: output bit<8>) {
                inst child: Child (clk, d, q);
            }
        "#;
    let mut image = crate::Simulator::builder(source, "Top")
        .four_state(false)
        .compile_native()
        .unwrap()
        .into_program_image();
    let previous = image.code_entries.len();
    image
        .code_entries
        .retain(|entry| !entry.name.starts_with(crate::clock_aliases::EXPORT_PREFIX));
    assert_eq!(previous, image.code_entries.len() + 1);
    let image =
        NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap()).unwrap();
    let mut sim = crate::Simulator::from_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    let event = sim.event("clk");
    let d = sim.signal("d");
    sim.set(d, 42u8);
    sim.active_edge(event).unwrap();
    sim.inactive_edge(event).unwrap();
    assert_eq!(sim.get(sim.signal("q")), 42u8.into());
    assert_eq!(
        sim.get(sim.child_signal(&[("child", 0)], "clk")),
        0u8.into()
    );
}

/// A unit of `blocks` blocks loading `value`, with its maps filled in
/// forward or reverse order.
fn unit(blocks: usize, value: u8, width: usize, reverse: bool) -> NativeUnit {
    let mut ids = (0..blocks).collect::<Vec<_>>();
    if reverse {
        ids.reverse();
    }
    NativeUnit {
        entry_block_id: BlockId(0),
        blocks: ids
            .iter()
            .map(|&id| {
                let terminator = if id + 1 < blocks {
                    SIRTerminator::Jump(BlockId(id + 1), vec![])
                } else {
                    SIRTerminator::Return
                };
                let block = BasicBlock {
                    id: BlockId(id),
                    params: vec![],
                    instructions: vec![SIRInstruction::Imm(RegisterId(id), SIRValue::new(value))],
                    terminator,
                };
                (BlockId(id), block)
            })
            .collect(),
        register_map: ids
            .iter()
            .map(|&id| (RegisterId(id), RegisterType::Logic { width }))
            .collect(),
    }
}

#[test]
fn unit_classes_match_deep_comparison() {
    let mut pool = Vec::new();
    for blocks in 1..4 {
        for value in 0..2 {
            for width in [1, 8] {
                for reverse in [false, true] {
                    pool.push(unit(blocks, value, width, reverse));
                }
            }
        }
    }
    let units = pool.iter().collect::<Vec<_>>();
    // Lists of up to two units, repeating the same references as the
    // shared prefix of comb-apply tasks does.
    let lists = std::iter::once(Vec::new())
        .chain(units.iter().map(|unit| vec![*unit]))
        .chain(
            units
                .iter()
                .flat_map(|a| units.iter().map(move |b| vec![*a, *b])),
        )
        .collect::<Vec<_>>();
    let mut classes = NativeUnitClasses::default();
    let keys = lists
        .iter()
        .map(|list| classes.classes(list))
        .collect::<Vec<_>>();
    for (a, key_a) in lists.iter().zip(&keys) {
        for (b, key_b) in lists.iter().zip(&keys) {
            assert_eq!(a == b, key_a == key_b, "{a:?} {b:?}");
        }
    }
    // The pool has equal units with differently ordered maps.
    assert!(classes.count < pool.len());
}
