Historical native evidence
==========================

These reports describe the original binaries and sources named in their provenance.
They do not validate the integrated runtime. The manifest records the reference
revision and SHA-256 of each retained report and source. Original paths and hashes
inside reports remain unchanged; archived source files have a `.txt` suffix.

The retained workflow regression explicitly selects these archived sources. Passing
a report path to `verify-workflow` checks current source files instead. Archived
hashes cannot satisfy that current-source check. Regression tests also reject missing
historical sources, altered hashes, and incorrect workflow outcomes.

Resource, controller-setup, traffic, freshness and screen regressions likewise select
archives explicitly. Current comparison source checks have no automatic archive
fallback. Screen regressions rebuild the historical corpus and negative cases from
the archived viewport fixtures, preserving their original hashes.

Headless reports and their original regression checks now use explicit archival
validation. Two baseline-build source versions remain unavailable and are listed in
`unresolved_historical_sources` in the manifest. The original baseline validator did
not verify that full inventory; passing retained tests do not resolve this limitation.
Current measurements and build evidence remain separate work.
