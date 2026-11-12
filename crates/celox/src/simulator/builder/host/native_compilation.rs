//! Native code compiled for a design, before it is loaded and initialized.

use super::*;

/// A code-generated native program that has not been loaded into
/// executable memory or initialized as a simulator yet.
///
/// Keeping compilation separate from initialization lets compiler-only
/// clients stop after native code generation without applying initial
/// values or executing the first combinational settle. `Target` is what
/// [`Self::initialize`] builds: a [`Simulator`], or a timed
/// [`crate::Simulation`] whose image carries the process kernels and the
/// compiled scheduler.
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
#[must_use]
pub struct NativeCompilation<Target = Simulator> {
    pub(super) image: crate::backend::native::NativeProgramImage,
    pub(super) program: LaidOutProgram,
    pub(super) warnings: Vec<CompilationWarning>,
    pub(super) options: SimulatorOptions,
    pub(super) vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
    pub(super) injected_components: crate::InjectedComponents,
    pub(super) _target: std::marker::PhantomData<Target>,
}

/// `Simulator` runs no process kernel, so a design with one would
/// simulate wrongly without any error.
pub(super) fn validate_untimed_processes(
    program: &crate::ir::RuntimeProgram,
) -> Result<(), SimulatorError> {
    let schema = &program.runtime_schema;
    if schema
        .processes
        .iter()
        .all(|process| process.kind == celox_design::ProcessKind::Callable)
    {
        return Ok(());
    }
    let origin = schema.process_origins.first().cloned().unwrap_or_default();
    Err(SimulatorError::new(
        SimulatorErrorKind::ProcessRequiresSimulation(origin),
    ))
}

/// A native image records waveforms only if its arrays are packed,
/// which an image compiled with VCD enabled has: a VCD file of the host
/// (`vcd_path`) or of the design (`$dumpvars`) needs one.
#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
pub(super) fn validate_native_vcd_layout(
    layout: &crate::MemoryLayout,
    program: &crate::ir::RuntimeProgram,
    vcd_path: &Option<(std::path::PathBuf, crate::WaveformFormat)>,
) -> Result<(), SimulatorError> {
    let source_vcd = program
        .runtime_schema
        .host_request_sites
        .iter()
        .any(|site| site.kind == celox_design::HostRequestKind::Vcd);
    if (vcd_path.is_some() || source_vcd)
        && layout.unpacked_arrays.values().any(|array| {
            !array.element_width.is_multiple_of(8)
                || array.element_stride != array.element_width / 8
        })
    {
        return Err(SimulatorError::from(crate::CodegenError::message(
            "VCD requires packed array storage; compile this native image with VCD enabled",
        )));
    }
    Ok(())
}

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
pub(super) fn initialize_native_backend(
    backend: crate::backend::native::NativeBackend,
    program: crate::ir::RuntimeProgram,
    warnings: Vec<CompilationWarning>,
    options: SimulatorOptions,
    vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
    injected_components: crate::InjectedComponents,
) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
    validate_untimed_processes(&program)?;
    let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
    sim.components.set_injected(injected_components);
    sim.diagnostics = options.diagnostics.clone();
    sim.set_plusargs(options.plusargs.clone());
    sim.set_file_io(celox_runtime::file_io::FileIo::new(
        options.file_system.create(),
    ));
    if let Err(error) = validate_native_vcd_layout(sim.layout(), &sim.program, &vcd_path) {
        return Err(error.with_warnings(std::mem::take(&mut sim.warnings)));
    }
    if let Some((path, format)) = vcd_path {
        let descs = sim.build_vcd_descs(options.four_state);
        let vcd_writer = crate::WaveformWriter::new_with_time_precision(
            path,
            format,
            &descs,
            sim.program.design.time_precision,
        )
        .map_err(|_| {
            SimulatorError::from(crate::RuntimeErrorCode::InternalError)
                .with_warnings(std::mem::take(&mut sim.warnings))
        })?;
        sim.vcd_writer = Some(vcd_writer);
    }
    let apply_initial_start = options.diagnostics.phase_timing.then(crate::timing::now);
    sim.apply_initial_values();
    if let Some(start) = apply_initial_start {
        tracing::debug!("[phase-timing] apply_initial_values: {:?}", start.elapsed());
    }
    let settle_start = options.diagnostics.phase_timing.then(crate::timing::now);
    sim.modify(|_| {}).map_err(|error| {
        SimulatorError::from(error).with_warnings(std::mem::take(&mut sim.warnings))
    })?;
    if let Some(start) = settle_start {
        tracing::debug!("[phase-timing] initial_settle: {:?}", start.elapsed());
    }
    Ok(sim)
}

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
#[allow(unreachable_code)]
pub(super) fn initialize_native_image(
    image: crate::backend::native::NativeProgramImage,
    program: LaidOutProgram,
    warnings: Vec<CompilationWarning>,
    options: SimulatorOptions,
    vcd_path: Option<(std::path::PathBuf, crate::WaveformFormat)>,
    injected_components: crate::InjectedComponents,
) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
    #[cfg(any(
        all(feature = "arm64-codegen", not(target_arch = "aarch64")),
        all(feature = "x86_64-codegen", not(target_arch = "x86_64"))
    ))]
    {
        let _ = (image, program, options, vcd_path, injected_components);
        return Err(
            SimulatorError::from(crate::RuntimeErrorCode::InternalError).with_warnings(warnings)
        );
    }

    // Safety: callers either produced the image in this process or loaded
    // it from an explicitly trusted native-image artifact.
    let backend = match unsafe {
        crate::backend::native::NativeBackend::from_image_with_dpi(image, &options.dpi)
    } {
        Ok(backend) => backend,
        Err(error) => return Err(error.with_warnings(warnings)),
    };
    initialize_native_backend(
        backend,
        program.into_runtime(),
        warnings,
        options,
        vcd_path,
        injected_components,
    )
}

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
impl<Target> NativeCompilation<Target> {
    /// Warnings emitted while compiling this artifact.
    pub fn warnings(&self) -> &[CompilationWarning] {
        &self.warnings
    }

    /// Pointer-free native machine-code artifact produced by this build.
    pub fn program_image(&self) -> &crate::backend::native::NativeProgramImage {
        &self.image
    }

    /// Consume the compilation and return its pointer-free native image.
    pub fn into_program_image(self) -> crate::backend::native::NativeProgramImage {
        self.image
    }

    /// Consume the compilation without initializing executable memory,
    /// retaining both the native artifact and its compilation warnings.
    pub fn into_program_image_and_warnings(
        self,
    ) -> (
        crate::backend::native::NativeProgramImage,
        Vec<CompilationWarning>,
    ) {
        (self.image, self.warnings)
    }

    /// Write the generated native image as a standalone binary container.
    pub fn write_image(
        &self,
        output_path: impl AsRef<std::path::Path>,
    ) -> Result<(), crate::backend::native::NativeImageContainerError> {
        self.image.write_container(output_path)
    }
}

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
impl NativeCompilation<Simulator> {
    /// Load the image into executable memory and initialize runtime state.
    pub fn initialize(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        let Self {
            image,
            program,
            warnings,
            options,
            vcd_path,
            injected_components,
            _target,
        } = self;
        initialize_native_image(
            image,
            program,
            warnings,
            options,
            vcd_path,
            injected_components,
        )
    }
}

#[cfg(any(
    target_arch = "x86_64",
    feature = "arm64-codegen",
    target_arch = "aarch64"
))]
impl NativeCompilation<crate::Simulation> {
    /// Load the image into executable memory and start a timed
    /// simulation of it.
    pub fn initialize(
        self,
    ) -> Result<crate::Simulation<crate::backend::native::NativeBackend>, SimulatorError> {
        let Self {
            image,
            program,
            warnings,
            options,
            vcd_path,
            injected_components,
            _target,
        } = self;
        SimulatorBuilder::<crate::Simulation>::start_native_image(
            image,
            program.into_runtime(),
            warnings,
            options,
            vcd_path,
            injected_components,
        )
    }
}
