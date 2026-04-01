/// Generates three `#[test]` functions (native, cranelift, wasm) per test.
///
/// Two forms, freely mixable in one invocation:
///
/// ```rust
/// all_backends! {
///     // Simple: no setup needed
///     fn test_a(sim) {
///         @build Simulator::builder(r#"..."#, "Top");
///         sim.modify(|io| io.set(sim.signal("a"), 1u8)).unwrap();
///     }
///
///     // With setup (variables survive into body):
///     fn test_b(sim) {
///         @setup { let code = format!("{SRC}\n{extra}"); }
///         @build Simulator::builder(&code, "Top");
///         sim.modify(|io| io.set(sim.signal("a"), 1u8)).unwrap();
///     }
/// }
/// ```
macro_rules! all_backends {
    // ── internal: implementation ─────────────────────────────────────
    (@impl
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {
        mod $name {
            use super::*;

            #[test]
            $(#[$meta])*
            #[allow(unused_mut, unused_variables)]
            fn native() {
                $($setup)*
                let mut $sim = { $builder }.build_native().unwrap();
                $($body)*
            }

            #[test]
            $(#[$meta])*
            #[allow(unused_mut, unused_variables)]
            fn cranelift() {
                $($setup)*
                let mut $sim = { $builder }.build_cranelift().unwrap();
                $($body)*
            }

            #[test]
            $(#[$meta])*
            #[allow(unused_mut, unused_variables)]
            fn wasm() {
                $($setup)*
                let mut $sim = { $builder }.build_wasm().unwrap();
                $($body)*
            }
        }
    };

    // ── internal: dispatch per body shape ────────────────────────────
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @setup { $($setup:tt)* } @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            setup { $($setup)* }
            build { $builder }
            body { $($body)* }
        );
    };

    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            setup { }
            build { $builder }
            body { $($body)* }
        );
    };

    // ── entry point ─────────────────────────────────────────────────
    ($(
        $(#[$meta:meta])*
        fn $name:ident($sim:ident) $body:tt
    )*) => {$(
        all_backends!(@dispatch
            $(#[$meta])* fn $name ($sim) $body
        );
    )*};
}
