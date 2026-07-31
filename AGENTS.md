# Agent Instructions

## Validate before pushing

The pre-push hook is only a fast minimum safety net. Before pushing, inspect the
actual change and autonomously run the formatting, checks, and tests needed to
validate it. Do not treat passing the hook as sufficient validation, and do not
defer tests to CI merely because the hook does not run them.

Choose focused tests that exercise the changed behavior, then broaden validation
in proportion to the change's scope and risk. If a required test cannot be run,
state that explicitly in the handoff instead of silently omitting it.

When a task has a clear requested outcome, pursue it without waiting for
step-by-step instructions. Inspect the repository, make reasonable assumptions,
implement the change, and continue while a safe, in-scope next step remains.

Do not stop after proposing a plan or making a partial change when the remaining
work can be discovered and completed locally. If a check fails, investigate and
fix failures caused by the change before reporting completion. Report assumptions,
validation performed, and any unresolved limitation in the final handoff.

Ask for direction only when a missing choice would materially change the result,
or when the next action needs additional authority, is destructive, or affects
systems outside the requested scope. Autonomy does not permit broadening the task,
bypassing safety checks, or committing, pushing, or publishing unless requested.
