#!/bin/sh
set -eu
FUX_RECORD_FIXTURES=1 cargo test --test verification_corpus prefix_literal_agrees_across_independent_interpreters_and_the_golden -- --exact
