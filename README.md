# fux and zor (ECS-native rewrite)

fux is a persistent terminal multiplexer whose session is a `bevy_ui` scene in a `bevy_ecs`
World; zor is the agent/task policy layer, a second Bevy App and a BRP client of fux.
Design: `docs/prompts/ecs-native-rewrite-prompt.md`; Bevy mechanisms: `docs/bevy-source-patterns.md`.
