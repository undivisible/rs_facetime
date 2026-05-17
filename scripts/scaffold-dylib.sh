#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HELPER_DIR="${ROOT}/helper"

mkdir -p "${HELPER_DIR}"
cat > "${HELPER_DIR}/README.md" <<'EOF'
# rs-facetime-bridge-helper (scaffold)

Implement an injected dylib for FaceTime.app that:

1. On load, creates `~/Library/Containers/com.apple.FaceTime/Data/.rs-facetime-bridge-ready`
2. Watches `Data/.rs-facetime-rpc/in/*.json` and writes matching `out/{id}.json`
3. Handles actions: `ping`, `status`, `start-call`, `end-call`

Use openclaw/imsg `Sources/IMsgHelper` as a structural reference (MIT).
Do not vendor BlueBubbles GPL server code.

Build one artifact per row in the root README dylib matrix.
EOF

echo "Scaffold written to ${HELPER_DIR}/README.md"
