#!/bin/sh
set -eu
FUX_RECORD_FIXTURES=1 cargo test --test verification_corpus -- --test-threads=1
