#!/bin/sh
# Builds and runs generated-buffer tests only. Never requests audio permission.
set -eu
cd "$(dirname "$0")/.."
test_dir=$(mktemp -d "${TMPDIR:-/tmp}/rs-facetime-audio-test.XXXXXX")
trap 'rm -f "$test_dir/native_ring" "$test_dir/native_cleanup"; rmdir "$test_dir"' EXIT HUP INT TERM
for test_name in native_ring native_cleanup; do
  xcrun --sdk macosx clang -std=c11 -fobjc-arc -Wall -Wextra -Werror \
    -mmacosx-version-min=14.0 -fsanitize=address,undefined \
    "tests/$test_name.m" -framework Foundation -framework CoreAudio \
    -o "$test_dir/$test_name"
  "$test_dir/$test_name"
done
