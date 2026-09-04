#!/bin/sh
set -eu
FUX_RECORD_FIXTURES=1 cargo test --test verification_corpus -- --test-threads=1
FUX_RECORD_FIXTURES=1 cargo test --test host \
  remote_viewer_reconnects_after_forced_loopback_loss_without_state_reset \
  -- --exact --test-threads=1
