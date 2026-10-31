// Ordinary PRs execute the native serial cases. Keep optional backend and
// parallel execution for direct changes to those implementations or full CI.
const BACKENDS = [
  {
    changes: /(?:^|[\/_.-])cranelift(?:[\/_.-]|$)/,
    packages: ["celox-backend-cranelift"],
    names: "cranelift|cranelift_parallel",
  },
  {
    changes: /(?:^|[\/_.-])(?:wasm|wasmtime|wasi)(?:[\/_.-]|$)/,
    packages: ["celox-backend-wasm", "celox-wasm"],
    names: "wasm|wasmtime|wasi",
  },
  {
    changes: /(?:^|[\/_.-])(?:parallel|threads|scheduler)(?:[\/_.-]|$)/,
    packages: [],
    names: "native_parallel|cranelift_parallel|parallel|threads",
  },
];

export function backendTestFilter(files, { full = false } = {}) {
  // Unknown diffs and explicit full validation retain all execution lanes.
  if (full || files === null || files.some((path) =>
    /(?:^|\/)Cargo\.(?:toml|lock)$/.test(path) ||
    /^(?:\.cargo\/|rust-toolchain(?:\.|$))/.test(path))) return "all()";
  const excluded = BACKENDS.filter(({ changes }) =>
    !files.some((path) => changes.test(path.replace(/^\.\//, ""))));
  const terms = excluded.flatMap(({ packages, names }) => [
    ...packages.map((name) => `package(${name})`),
    `binary_id(/(^|::|_|-)(${names})(::|_|-|$)/)`,
    `test(/(^|::|_)(${names})(::|_|$)/)`,
  ]);
  return terms.length ? `not (${terms.join(" | ")})` : "all()";
}

export function intersectTestFilters(scope, backend = "all()") {
  return backend === "all()" ? scope : `(${scope}) & (${backend})`;
}
