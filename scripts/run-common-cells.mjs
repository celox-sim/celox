import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";

export const revision = readFileSync(
  new URL("../conformance/common-cells/revision", import.meta.url), "utf8",
).trim();
const sources = [
  "src/cc_binary_to_gray.sv",
  "src/cc_gray_to_binary.sv",
  "test/cc_graycode_tb.sv",
];

export function run({ source, celox, output }) {
  source = resolve(source);
  celox = resolve(celox);
  output = resolve(output);
  mkdirSync(output, { recursive: true });
  rmSync(join(output, "result.json"), { force: true });
  const head = spawnSync("git", ["-C", source, "rev-parse", "HEAD"], { encoding: "utf8" });
  if (head.status !== 0 || head.stdout.trim() !== revision) {
    throw new Error(`Expected common_cells revision ${revision}; fetch the pinned checkout first`);
  }
  const manifest = (files, top = "cc_graycode_tb") => `sources = [${files.map((file) => JSON.stringify(file)).join(", ")}]\ntop = ${JSON.stringify(top)}\n`;
  const original = sources.map((file) => join(source, file));
  const project = join(output, "celox.toml");
  writeFileSync(project, manifest(original));
  const execute = (name, projectFile, negative = false, diagnostic = "assert failed") => {
    const result = spawnSync(celox, ["run", "--project", projectFile], {
      encoding: "utf8", timeout: 120_000, maxBuffer: 8 * 1024 * 1024,
    });
    const log = `${result.stdout ?? ""}${result.stderr ?? ""}`;
    writeFileSync(join(output, `${name}.log`), log);
    if (result.error || result.signal || (negative
      ? result.status !== 1 || !log.includes(diagnostic)
        || !log.includes("SystemVerilog testbench reported an assertion failure or $error")
      : result.status !== 0)) {
      throw new Error(`${name} failed (exit ${result.status}, signal ${result.signal}): ${result.error ?? log}`);
    }
  };
  execute("graycode", project);

  // Mutate only a separate RTL copy: prove that the upstream assertions execute.
  const text = readFileSync(original[0], "utf8");
  const assignment = "assign z_o   = a_i ^ a_ext[Width:1];";
  if (text.split(assignment).length !== 2) throw new Error("Cannot locate the pinned Gray code assignment");
  const broken = join(output, "negative_binary_to_gray.sv");
  writeFileSync(broken, text.replace(assignment, "assign z_o   = '0;"));
  const negativeProject = join(output, "negative.toml");
  writeFileSync(negativeProject, manifest([broken, ...original.slice(1)]));
  execute("graycode-negative", negativeProject, true);
  const popcount = join(source, "src/cc_popcount.sv");
  const popcountTb = join(source, "test/cc_popcount_tb.sv");
  const popcountProject = join(output, "popcount.toml");
  writeFileSync(popcountProject, manifest([popcount, popcountTb], "cc_popcount_tb"));
  execute("popcount", popcountProject);

  const popcountText = readFileSync(popcount, "utf8");
  const increment = "popcount_o += data_i[i];";
  if (popcountText.split(increment).length !== 2) throw new Error("Cannot locate the pinned popcount accumulation");
  const brokenPopcount = join(output, "negative_popcount.sv");
  writeFileSync(brokenPopcount, popcountText.replace(increment, "popcount_o += 0;"));
  const negativePopcountProject = join(output, "popcount-negative.toml");
  writeFileSync(negativePopcountProject, manifest([brokenPopcount, popcountTb], "cc_popcount_tb"));
  execute("popcount-negative", negativePopcountProject, true, "Popcount of");
  writeFileSync(join(output, "result.json"), `${JSON.stringify({
    revision, top: "cc_graycode_tb", positive: "passed", negative: "assertion failure detected",
    testbenches: ["cc_graycode_tb", "cc_popcount_tb"],
  }, null, 2)}\n`);
  console.log(`common_cells ${revision}: upstream Gray code and popcount TBs passed; both broken RTL copies rejected`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.argv.length !== 5) {
    console.error("Usage: node scripts/run-common-cells.mjs SOURCE CELOX_BIN OUTPUT_DIR");
    process.exitCode = 1;
  } else {
    try {
      run({ source: process.argv[2], celox: process.argv[3], output: process.argv[4] });
    } catch (error) {
      console.error(error.message);
      process.exitCode = 1;
    }
  }
}
