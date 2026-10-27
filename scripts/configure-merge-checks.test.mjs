import assert from "node:assert/strict";
import test from "node:test";
import { lightweightRuleset } from "./configure-merge-checks.mjs";

test("migration removes only deferred platforms, preserves enforcement and adds the queue gate", () => {
  const input = {
    id: 123, name: "master", target: "branch", enforcement: "active",
    bypass_actors: [{ actor_id: 42, actor_type: "Integration", bypass_mode: "always" }],
    conditions: { ref_name: { include: ["refs/heads/master"], exclude: [] } },
    rules: [{ type: "non_fast_forward" }, { type: "required_status_checks", parameters: {
      strict_required_status_checks_policy: false,
      required_status_checks: ["Lint & Format", "Rust Test & NAPI Build", "Conventional Commit title", "NAPI Build (Windows)", "JS Test (Windows)", "NAPI Build (Linux ARM64 GNU)", "Custom audit"]
        .map(context => ({ context, integration_id: 15368 })),
    } }],
  };
  const before = structuredClone(input);
  const actual = lightweightRuleset(input);
  assert.deepEqual(input, before);
  assert.equal(actual.enforcement, "active");
  assert.deepEqual(actual.bypass_actors, input.bypass_actors);
  assert.deepEqual(actual.conditions, input.conditions);
  assert.deepEqual(actual.rules[0], input.rules[0]);
  assert.deepEqual(actual.rules[1].parameters.required_status_checks.map(check => check.context),
    ["Lint & Format", "Rust Test & NAPI Build", "Conventional Commit title", "Custom audit", "Queue validation"]);
  assert.deepEqual(lightweightRuleset(actual), actual);
  assert.equal(actual.id, undefined);
});

test("unexpected rulesets are rejected instead of guessing required checks", () => {
  assert.throws(() => lightweightRuleset({ rules: [] }), /Missing required/);
  assert.throws(() => lightweightRuleset({ rules: [{ type: "required_status_checks", parameters: { required_status_checks: [] } }] }), /Missing GitHub Actions/);
});
