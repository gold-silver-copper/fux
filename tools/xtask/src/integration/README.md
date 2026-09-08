reload-plugin.js.txt and reload-exec.py.txt retain the exact instrumentation bytes
from capture_opencode_integration.py for validation of historical JSON hashes.
They are archival text only and are not executed by the Rust validator. The
capture script remains pending migration; a future Rust capture must record its
own instrumentation provenance rather than claiming these historical hashes.
