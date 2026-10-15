import assert from "node:assert/strict";
import test from "node:test";
import { checkQueueTitle } from "./check-mergify-queue-title.mjs";

const input = { repository: "o/r", number: 99, sha: "a".repeat(40) };
const open = { state: "open", head: { sha: input.sha } };
const closed = { ...open, state: "closed" };
const replaced = { ...open, head: { sha: "b".repeat(40) } };
const missing = new Error("No merge queue metadata found");

for (const stale of [closed, replaced]) {
  test(`skip ${stale.state === "closed" ? "closed" : "replaced"} batch before reading deleted metadata`, async () => {
    const result = await checkQueueTitle(input, {
      api: async () => stale,
      readMetadata: () => assert.fail("must not read obsolete notes"),
      validate: () => assert.fail("must not validate obsolete batch"),
    });
    assert.match(result, /Skipped/);
  });
  test(`handle ${stale.state === "closed" ? "closure" : "replacement"} during metadata retrieval`, async () => {
    let reads = 0;
    const result = await checkQueueTitle(input, {
      api: async () => (++reads === 1 ? open : stale),
      readMetadata: () => {
        throw missing;
      },
    });
    assert.equal(reads, 2);
    assert.match(result, /Skipped/);
  });
}

test("missing metadata for an active batch remains a failure", async () => {
  await assert.rejects(
    checkQueueTitle(input, {
      api: async () => open,
      readMetadata: () => {
        throw missing;
      },
    }),
    (error) => error === missing,
  );
});

test("active batch validates the exact CLI metadata", async () => {
  const metadata = { pull_requests: [{ number: 12 }] };
  let validated = false;
  await checkQueueTitle(input, {
    api: async () => open,
    readMetadata: () => metadata,
    validate: async (value) => {
      assert.deepEqual(value, { ...input, metadata });
      validated = true;
    },
  });
  assert.equal(validated, true);
});

test("invalid title or release impact on an active batch remains a failure", async () => {
  await assert.rejects(
    checkQueueTitle(input, {
      api: async () => open,
      readMetadata: () => ({}),
      validate: async () => {
        throw new Error("Invalid title");
      },
    }),
    /Invalid title/,
  );
});

test("an API outage during the race check cannot turn failure into success", async () => {
  let reads = 0;
  await assert.rejects(
    checkQueueTitle(input, {
      api: async () => {
        if (++reads === 1) return open;
        throw new Error("API unavailable");
      },
      readMetadata: () => {
        throw missing;
      },
    }),
    /API unavailable/,
  );
});

test("unknown API state and invalid event SHAs fail closed", async () => {
  await assert.rejects(
    checkQueueTitle(input, { api: async () => ({}) }),
    /current queue PR state/,
  );
  await assert.rejects(
    checkQueueTitle({ ...input, sha: "main" }),
    /Exact batch SHA/,
  );
});
