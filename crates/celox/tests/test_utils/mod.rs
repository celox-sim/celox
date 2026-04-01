/// Generates three `#[test]` functions (native, cranelift, wasm) per test.
///
/// ```rust
/// all_backends! {
///     // Simple
///     fn test_a(sim) {
///         @build Simulator::builder(r#"..."#, "Top");
///         assert_eq!(sim.get(sim.signal("o")), 1u8.into());
///     }
///
///     // With setup (variables survive into body)
///     fn test_b(sim) {
///         @setup { let code = format!("{SRC}\n{extra}"); }
///         @build Simulator::builder(&code, "Top");
///         assert_eq!(sim.get(sim.signal("o")), 1u8.into());
///     }
///
///     // Ignore specific backends (known limitations)
///     fn test_c(sim) {
///         @ignore_on(wasm);
///         @build Simulator::builder(r#"..."#, "Top");
///         assert_eq!(sim.get(sim.signal("o")), 1u8.into());
///     }
///
///     fn test_d(sim) {
///         @ignore_on(wasm, cranelift);
///         @setup { let code = format!("..."); }
///         @build Simulator::builder(&code, "Top");
///         assert_eq!(sim.get(sim.signal("o")), 1u8.into());
///     }
/// }
/// ```
macro_rules! all_backends {
    // ── internal: implementation with per-backend ignore ─────────────
    (@impl
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        native_extra { $(#[$na:meta])* }
        cranelift_extra { $(#[$ca:meta])* }
        wasm_extra { $(#[$wa:meta])* }
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {
        mod $name {
            use super::*;

            #[test]
            $(#[$meta])*
            $(#[$na])*
            #[allow(unused_mut, unused_variables)]
            fn native() {
                $($setup)*
                let mut $sim = { $builder }.build_native().unwrap();
                $($body)*
            }

            #[test]
            $(#[$meta])*
            $(#[$ca])*
            #[allow(unused_mut, unused_variables)]
            fn cranelift() {
                $($setup)*
                let mut $sim = { $builder }.build_cranelift().unwrap();
                $($body)*
            }

            #[test]
            $(#[$meta])*
            $(#[$wa])*
            #[allow(unused_mut, unused_variables)]
            fn wasm() {
                $($setup)*
                let mut $sim = { $builder }.build_wasm().unwrap();
                $($body)*
            }
        }
    };

    // ── internal: resolve @ignore_on into per-backend extra attrs ────
    (@resolve_ignore (wasm) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { }
            cranelift_extra { }
            wasm_extra { #[ignore] }
            $($rest)*
        );
    };
    (@resolve_ignore (cranelift) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { }
            cranelift_extra { #[ignore] }
            wasm_extra { }
            $($rest)*
        );
    };
    (@resolve_ignore (native) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { #[ignore] }
            cranelift_extra { }
            wasm_extra { }
            $($rest)*
        );
    };
    (@resolve_ignore (wasm, cranelift) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { }
            cranelift_extra { #[ignore] }
            wasm_extra { #[ignore] }
            $($rest)*
        );
    };
    (@resolve_ignore (cranelift, wasm) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { }
            cranelift_extra { #[ignore] }
            wasm_extra { #[ignore] }
            $($rest)*
        );
    };
    (@resolve_ignore (native, wasm) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { #[ignore] }
            cranelift_extra { }
            wasm_extra { #[ignore] }
            $($rest)*
        );
    };
    (@resolve_ignore (wasm, native) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { #[ignore] }
            cranelift_extra { }
            wasm_extra { #[ignore] }
            $($rest)*
        );
    };
    (@resolve_ignore (native, cranelift) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { #[ignore] }
            cranelift_extra { #[ignore] }
            wasm_extra { }
            $($rest)*
        );
    };
    (@resolve_ignore (cranelift, native) -> $($rest:tt)*) => {
        all_backends!(@impl_with_ignore
            native_extra { #[ignore] }
            cranelift_extra { #[ignore] }
            wasm_extra { }
            $($rest)*
        );
    };

    // ── internal: @impl_with_ignore → @impl passthrough ─────────────
    (@impl_with_ignore
        native_extra { $(#[$na:meta])* }
        cranelift_extra { $(#[$ca:meta])* }
        wasm_extra { $(#[$wa:meta])* }
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            native_extra { $(#[$na])* }
            cranelift_extra { $(#[$ca])* }
            wasm_extra { $(#[$wa])* }
            setup { $($setup)* }
            build { $builder }
            body { $($body)* }
        );
    };

    // ── internal: dispatch per body shape ────────────────────────────

    // @ignore_on + @setup + @build
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @ignore_on $ignore_list:tt; @setup { $($setup:tt)* } @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@resolve_ignore $ignore_list ->
            $(#[$meta])* fn $name ($sim)
            setup { $($setup)* }
            build { $builder }
            body { $($body)* }
        );
    };

    // @ignore_on + @build (no setup)
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @ignore_on $ignore_list:tt; @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@resolve_ignore $ignore_list ->
            $(#[$meta])* fn $name ($sim)
            setup { }
            build { $builder }
            body { $($body)* }
        );
    };

    // @setup + @build (no ignore)
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @setup { $($setup:tt)* } @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            native_extra { }
            cranelift_extra { }
            wasm_extra { }
            setup { $($setup)* }
            build { $builder }
            body { $($body)* }
        );
    };

    // @build only (no ignore, no setup)
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            native_extra { }
            cranelift_extra { }
            wasm_extra { }
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
