pub mod veryl_sim;
pub mod veryl_sv;

#[path = "../fixtures/veryl_std.rs"]
#[allow(dead_code)]
pub mod veryl_std;

/// Generates native, cranelift, wasm, interpreter, and Veryl reference tests,
/// plus an SV frontend test when the `systemverilog` feature is enabled.
///
/// The `interp` arm calls `build_interpreter()`, which executes every
/// execution unit on the Tier-0 SIR interpreter against the same memory
/// image ABI as the compiled backends.
///
/// ```rust
/// all_backends! {
///     fn test_a(sim) {
///         @build Simulator::builder(r#"..."#, "Top");
///         assert_eq!(sim.get(sim.signal("o")), 1u8.into());
///     }
/// }
/// ```
macro_rules! all_backends {
    // ── internal: add #[ignore] when a backend is in the ignore list ──
    (@with_ignore native; (); { $($item:tt)* }) => { $($item)* };
    (@with_ignore native; (native $(, $rest:ident)*); { $($item:tt)* }) => {
        #[ignore]
        $($item)*
    };
    (@with_ignore native; ($first:ident $(, $rest:ident)*); { $($item:tt)* }) => {
        all_backends!(@with_ignore native; ($($rest),*); { $($item)* });
    };

    (@with_ignore cranelift; (); { $($item:tt)* }) => { $($item)* };
    (@with_ignore cranelift; (cranelift $(, $rest:ident)*); { $($item:tt)* }) => {
        #[ignore]
        $($item)*
    };
    (@with_ignore cranelift; ($first:ident $(, $rest:ident)*); { $($item:tt)* }) => {
        all_backends!(@with_ignore cranelift; ($($rest),*); { $($item)* });
    };

    (@with_ignore wasm; (); { $($item:tt)* }) => { $($item)* };
    (@with_ignore wasm; (wasm $(, $rest:ident)*); { $($item:tt)* }) => {
        #[ignore]
        $($item)*
    };
    (@with_ignore wasm; ($first:ident $(, $rest:ident)*); { $($item:tt)* }) => {
        all_backends!(@with_ignore wasm; ($($rest),*); { $($item)* });
    };

    (@with_ignore sv; (); { $($item:tt)* }) => { $($item)* };
    (@with_ignore sv; (sv $(, $rest:ident)*); { $($item:tt)* }) => {
        #[ignore]
        $($item)*
    };
    (@with_ignore sv; ($first:ident $(, $rest:ident)*); { $($item:tt)* }) => {
        all_backends!(@with_ignore sv; ($($rest),*); { $($item)* });
    };

    (@with_ignore veryl; (); { $($item:tt)* }) => { $($item)* };
    (@with_ignore veryl; (veryl $(, $rest:ident)*); { $($item:tt)* }) => {
        #[ignore]
        $($item)*
    };
    (@with_ignore veryl; ($first:ident $(, $rest:ident)*); { $($item:tt)* }) => {
        all_backends!(@with_ignore veryl; ($($rest),*); { $($item)* });
    };

    (@with_ignore interp; (); { $($item:tt)* }) => { $($item)* };
    (@with_ignore interp; (interp $(, $rest:ident)*); { $($item:tt)* }) => {
        #[ignore]
        $($item)*
    };
    (@with_ignore interp; ($first:ident $(, $rest:ident)*); { $($item:tt)* }) => {
        all_backends!(@with_ignore interp; ($($rest),*); { $($item)* });
    };

    // ── internal: emit the SV frontend test ─────────────────────────
    (@sv_fn
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        ignore_list { $ignore_list:tt }
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {
        all_backends!(@with_ignore sv; $ignore_list; {
            #[cfg(feature = "systemverilog")]
            #[test]
            $(#[$meta])*
            #[allow(unused_mut, unused_variables)]
            fn sv() {
                $($setup)*
                let __builder = { $builder };
                let __emitted = test_utils::veryl_sv::emit_veryl_sources(__builder.sources());
                let __sv_sources = __emitted.as_sv_sources();
                let mut $sim = celox::Simulator::from_sv_sources(
                    __sv_sources,
                    __builder.top(),
                )
                .four_state(__builder.four_state_enabled())
                .build()
                .unwrap();
                $($body)*
            }
        });
    };

    // ── internal: emit the Veryl reference test ─────────────────────
    (@veryl_fn
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        ignore_list { $ignore_list:tt }
        emit
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {
        all_backends!(@with_ignore veryl; $ignore_list; {
            #[test]
            $(#[$meta])*
            #[allow(unused_mut, unused_variables)]
            fn veryl() {
                $($setup)*
                let __builder = { $builder };
                let mut $sim = test_utils::veryl_sim::build_veryl_adapter(
                    __builder.sources(),
                    __builder.top(),
                    __builder.four_state_enabled(),
                );
                $($body)*
            }
        });
    };
    (@veryl_fn
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        ignore_list { $ignore_list:tt }
        skip
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {};

    // ── internal: generate each backend ─────────────────────────────
    (@impl
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        ignore_list { $ignore_list:tt }
        veryl_mode { $veryl_mode:ident }
        setup { $($setup:tt)* }
        build { $builder:expr }
        body { $($body:tt)* }
    ) => {
        mod $name {
            use super::*;

            all_backends!(@with_ignore native; $ignore_list; {
                #[test]
                #[cfg(any(
                    target_arch = "x86_64",
                    target_arch = "aarch64"
                ))]
                $(#[$meta])*
                #[allow(unused_mut, unused_variables)]
                fn native() {
                    $($setup)*
                    let mut $sim = { $builder }.build_native().unwrap();
                    $($body)*
                }
            });

            all_backends!(@with_ignore cranelift; $ignore_list; {
                #[test]
                $(#[$meta])*
                #[allow(unused_mut, unused_variables)]
                fn cranelift() {
                    $($setup)*
                    let mut $sim = { $builder }.build_cranelift().unwrap();
                    $($body)*
                }
            });

            all_backends!(@with_ignore wasm; $ignore_list; {
                #[test]
                $(#[$meta])*
                #[allow(unused_mut, unused_variables)]
                fn wasm() {
                    $($setup)*
                    let mut $sim = { $builder }.build_wasm().unwrap();
                    $($body)*
                }
            });

            all_backends!(@with_ignore interp; $ignore_list; {
                #[test]
                $(#[$meta])*
                #[allow(unused_mut, unused_variables)]
                fn interp() {
                    $($setup)*
                    let mut $sim = { $builder }.build_interpreter().unwrap();
                    $($body)*
                }
            });

            all_backends!(@sv_fn
                $(#[$meta])* fn $name ($sim)
                ignore_list { $ignore_list }
                setup { $($setup)* }
                build { $builder }
                body { $($body)* }
            );

            all_backends!(@veryl_fn
                $(#[$meta])* fn $name ($sim)
                ignore_list { $ignore_list }
                $veryl_mode
                setup { $($setup)* }
                build { $builder }
                body { $($body)* }
            );
        }
    };

    // ── internal: dispatch per body shape ───────────────────────────

    // @omit_veryl + @ignore_on + @setup + @build
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @omit_veryl; @ignore_on $ignore_list:tt; @setup { $($setup:tt)* } @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            ignore_list { $ignore_list }
            veryl_mode { skip }
            setup { $($setup)* }
            build { $builder }
            body { $($body)* }
        );
    };

    // @omit_veryl + @ignore_on + @build (no setup)
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @omit_veryl; @ignore_on $ignore_list:tt; @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            ignore_list { $ignore_list }
            veryl_mode { skip }
            setup { }
            build { $builder }
            body { $($body)* }
        );
    };

    // @omit_veryl + @setup + @build
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @omit_veryl; @setup { $($setup:tt)* } @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            ignore_list { () }
            veryl_mode { skip }
            setup { $($setup)* }
            build { $builder }
            body { $($body)* }
        );
    };

    // @omit_veryl + @build (no setup)
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @omit_veryl; @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            ignore_list { () }
            veryl_mode { skip }
            setup { }
            build { $builder }
            body { $($body)* }
        );
    };

    // @ignore_on + @setup + @build
    (@dispatch
        $(#[$meta:meta])* fn $name:ident ($sim:ident)
        { @ignore_on $ignore_list:tt; @setup { $($setup:tt)* } @build $builder:expr; $($body:tt)* }
    ) => {
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            ignore_list { $ignore_list }
            veryl_mode { emit }
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
        all_backends!(@impl
            $(#[$meta])* fn $name ($sim)
            ignore_list { $ignore_list }
            veryl_mode { emit }
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
            ignore_list { () }
            veryl_mode { emit }
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
            ignore_list { () }
            veryl_mode { emit }
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
