Stop expanding the project. Finish the existing herdr-gap-closure objective against a finite, evidence-backed completion checklist.

Keep the architecture fixed:
- fux is only a multiplexer with a good generic API.
- zor owns all agent behavior, orchestration, recovery, verification, UI, and notifications.
- koh owns transport, identity, and authorization.

First, audit the current implementation against herdr-gap-closure-prompt.md. Produce one concise checklist showing:
1. Completed requirements and their evidence.
2. Actual missing requirements.
3. Environment-blocked verification.
4. Optional improvements that are outside the completion path.

Preserve the original requirements, but do not turn every limitation, speculative edge case, or possible enhancement into another requirement. Resolve stale ledger entries from current evidence. Define concrete acceptance criteria for each remaining item and a clear stopping condition.

Then execute the remaining required work in priority order:
- Prioritize usable, reliable agent workflows and meaningful comparisons against herdr.
- Close the most consequential capability gaps before adding polish.
- Do not add new features merely because they are adjacent to the current work.
- Fix confirmed correctness and safety defects; avoid speculative hardening.
- Keep existing user changes intact.

Change the verification approach:
- Address the recurring journal-contention pattern coherently across affected fixtures, respecting each operation’s retry semantics. Never blindly retry a non-idempotent operation.
- Run targeted checks while implementing and batch related fixes before broader verification.
- Reuse applicable results when their tested source and assumptions remain unchanged.
- Run the full reconstructed integration gate at meaningful milestones and on the final source, rather than after every small change.
- Record known environmental failures separately. Do not repeatedly investigate or rerun the same blocked check without a relevant change, except where the final gate requires it.
- Keep required integrations mandatory; do not hide failures or weaken assertions to obtain green results.

Make comparative claims narrowly and honestly. Demonstrate the required improvements with reproducible scenarios; distinguish superiority, parity, unsupported behavior, and unverified coverage.

Report progress by completed acceptance criteria and remaining work—not by the number of edits or tests run. Give me the finite checklist first, then continue executing without asking for routine confirmation.

Finish with a requirement-by-requirement handoff, exact verification results, and genuine blockers. Do not declare the full objective complete until its required outcomes are established, and do not keep extending the completion checklist with optional work.
