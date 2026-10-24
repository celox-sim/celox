import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

function currentReport(root) {
  return resolve(root, "target/nextest/ci/junit.xml");
}

// Only remove nextest's report; never create target/ before Cargo/mbx runs.
export function clearCurrentReport(source = currentReport(process.cwd())) {
  rmSync(source, { force: true });
}

export function resetReports(directory, source = currentReport(process.cwd())) {
  clearCurrentReport(source);
  if (!existsSync(directory)) return;
  for (const name of readdirSync(directory)) {
    if (/^nextest-(?:full|cocotb-\d+|\d+)\.xml$/.test(name)) rmSync(resolve(directory, name));
  }
}

export function saveCurrentReport(directory, name, source = currentReport(process.cwd())) {
  if (!/^(?:full|cocotb-\d+|\d+)$/.test(name)) throw new Error("Invalid Rust report name");
  // A build failure or an empty selection may produce no report. Keep the
  // Cargo exit status and any earlier reports; never reuse a previous result.
  if (!existsSync(source)) return;
  mkdirSync(directory, { recursive: true });
  const destination = resolve(directory, `nextest-${name}.xml`);
  copyFileSync(source, destination);
  rmSync(source);
  return destination;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const directory = process.env.CELOX_CI_REPORT_DIR;
  if (!directory) throw new Error("CELOX_CI_REPORT_DIR is required");
  if (process.argv[2] === "reset") resetReports(directory);
  else if (process.argv[2] === "collect") saveCurrentReport(directory, "full");
  else throw new Error("Expected reset or collect");
}
