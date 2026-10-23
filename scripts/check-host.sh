#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
test_dir=$(mktemp -d)
# Test binaries are deliberately outside the repository and contain no secrets.
for module in control stream retry; do
  rustc +stable --edition=2021 --test "src/$module.rs" -o "$test_dir/$module-tests"
  "$test_dir/$module-tests"
done
