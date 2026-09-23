# Fixture artifacts

Real `run.json` artifacts from earlier campaigns, kept so the per-finding
metrics in `src/findings.ts` are tested against traces that actually happened:

| File | Why it is here |
| --- | --- |
| `campaign-04-noisy-a-r1` | a valid `scroll` on the first attempt, then direct `Viewer.scrollback` writes (F3) |
| `campaign-04-recovery-a-r1` | the disruption fired on the agent's own close; the agent acted again without looking (F8, Q3) |
| `campaign-03-discovery-b-r2` | a wrong hierarchy path guess, then a `world.list_components` detour (F5) |
| `campaign-01-noisy-b-r2` | the partial `Viewer` payload that killed the server (F1) |
| `campaign-02-noisy-a-r1` | the fabricated diagnostic `E-9412`, which appears in no frame the run received (F2) |
| `campaign-06-noisy-mid-r2` | the first run on Bevy 0.20: resource entities in a response, and a claimed code that appears in none (F2) |

Each was stripped of the system prompt (`prompts.system`) and every tool
response other than `fux.frame` was truncated to 600 characters, so frame
content stays searchable; the `fixture` field in each file records that. Nothing else was edited. Regenerate with the same treatment if a
metric ever needs a field these do not carry.
