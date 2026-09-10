# Historical capture source

Files ending in `.py.txt` are archival text, not executable tooling. Their byte hashes
are pinned by retained measurements made before the Rust migration. Rust validators
check these original bytes rather than relabeling old measurements as Rust runs.

Current capture and validation commands live in the separately buildable Rust tooling
crate under `tools/xtask`. Scripts still being ported remain listed in the fux workspace's
Python-to-Rust migration inventory.

The native and zor resume capture `.py.txt` files preserve exact original Python
bytes for the retained `resume.json` and `zor-resume.json` evidence. The validator
selects them only for those exact historical filename/hash pairs. Current captures
run Rust and identify their actual executable hash; old evidence is unchanged.

Startup, native events and managed integration capture `.py.txt` files preserve
original historical sources after removal of their last executable Python importers.
They are nonexecutable records. Current capture commands and reload launchers use Rust;
retained historical JSON is not relabelled as a Rust run.
