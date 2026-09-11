# Changelog

## 0.1.0 - 2026-09-11

- First release: the standalone PTY wrapper formerly built into `zor` (`zor <command>`), now
  `zor-wrap <command>`. Runs a child in a PTY, observes its screen with zor's rules and
  publishes agent state as OSC 7877. Independent of fux.
