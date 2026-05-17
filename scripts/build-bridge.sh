#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_LIB="${INSTALL_LIB:-}"

make -C "${ROOT}/helper" all

if [[ -n "${INSTALL_LIB}" ]]; then
  mkdir -p "${INSTALL_LIB}"
  cp "${ROOT}/lib/rs-facetime-bridge-helper.dylib" "${INSTALL_LIB}/"
  echo "Installed ${INSTALL_LIB}/rs-facetime-bridge-helper.dylib"
fi

echo "Built ${ROOT}/lib/rs-facetime-bridge-helper.dylib"
