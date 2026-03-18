#!/usr/bin/env bash
# compile-http1c-epf.sh — Compile EPF from XML sources
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROCESSOR_DIR="$REPO_ROOT/http-1c-dp"

cd "$REPO_ROOT"

oscript ./build/onescript/compile-external-processor.os "$PROCESSOR_DIR" "$REPO_ROOT"
