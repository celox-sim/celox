import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const DEFERRED_CHECKS = new Set([
  "NAPI Build (Linux ARM64 GNU)",
  "NAPI Build (Windows)",
  "JS Test (Windows)",
]);

// Preserve the ruleset's enforcement, bypass actors, branch selectors and all
// unrelated checks. Queue validation still rejects failures in its producers.
export function lightweightRuleset(ruleset) {
  const result = structuredClone(ruleset);
  const checks = result.rules.find(rule => rule.type === "required_status_checks");
  if (!checks) throw new Error("Missing required status checks rule");
  const previous = checks.parameters.required_status_checks;
  const integration = previous.find(check => check.context === "Lint & Format");
  if (!integration) throw new Error("Missing GitHub Actions lint check");
  checks.parameters.required_status_checks = previous.filter(check => !DEFERRED_CHECKS.has(check.context));
  if (!previous.some(check => check.context === "Queue validation")) {
    checks.parameters.required_status_checks.push({
      context: "Queue validation",
      integration_id: integration.integration_id,
    });
  }
  return Object.fromEntries(["name", "target", "enforcement", "bypass_actors", "conditions", "rules"]
    .filter(key => key in result).map(key => [key, result[key]]));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  if (args.some(arg => arg !== "--apply")) throw new Error("Usage: node scripts/configure-merge-checks.mjs [--apply]");
  const repository = process.env.GITHUB_REPOSITORY || "celox-sim/celox";
  const api = (path, options = []) => JSON.parse(execFileSync("gh", ["api", `repos/${repository}/${path}`, ...options], { encoding: "utf8" }));
  const rulesets = api("rulesets");
  const master = rulesets.find(rule => rule.name === "master");
  if (!master) throw new Error("master ruleset not found");
  const current = api(`rulesets/${master.id}`);
  const desired = lightweightRuleset(current);
  console.log(JSON.stringify(desired, null, 2));
  if (args.includes("--apply")) {
    // Do not remove existing required checks before the replacement is deployed.
    const source = path => Buffer.from(api(`contents/${path}?ref=master`).content, "base64").toString();
    const ci = source(".github/workflows/ci.yml");
    if (!ci.includes("name: Run merge queue integration tests") || !ci.includes("name: Release qualification")) {
      throw new Error("Merge the release-validation workflow change before applying the ruleset migration");
    }
    for (const path of [".github/workflows/ci-napi.yml", ".github/workflows/publish-crates.yml"]) {
      if (!source(path).includes("name: Verify release branch qualification")) {
        throw new Error(`${path} does not yet require release qualification`);
      }
    }
    execFileSync("gh", ["api", "--method", "PUT", `repos/${repository}/rulesets/${master.id}`, "--input", "-"], {
      input: JSON.stringify(desired), stdio: ["pipe", "inherit", "inherit"],
    });
  }
}
