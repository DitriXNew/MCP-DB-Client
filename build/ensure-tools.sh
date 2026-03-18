#!/usr/bin/env bash
# ensure-tools.sh
# Ensures cmake and ninja are available, downloading to build/tools/ if needed.
# Sets CMAKE variable and adds tools to PATH.
# Source this script (. ./ensure-tools.sh) so variables leak to the calling script.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLS_DIR="$SCRIPT_DIR/tools"

# ---- CMake ----
CMAKE="$(command -v cmake 2>/dev/null || true)"
if [[ -z "$CMAKE" && -x "$TOOLS_DIR/cmake/bin/cmake" ]]; then
    CMAKE="$TOOLS_DIR/cmake/bin/cmake"
fi

if [[ -z "$CMAKE" ]]; then
    echo "CMake not found — downloading to $TOOLS_DIR/cmake/ ..."
    mkdir -p "$TOOLS_DIR"
    TAG=$(curl -sL https://api.github.com/repos/Kitware/CMake/releases/latest | grep '"tag_name"' | head -1 | sed 's/.*"v\(.*\)".*/\1/')
    URL="https://github.com/Kitware/CMake/releases/download/v${TAG}/cmake-${TAG}-windows-x86_64.zip"
    echo "Downloading cmake $TAG ..."
    curl -sL "$URL" -o "$TOOLS_DIR/cmake.zip"
    mkdir -p "$TOOLS_DIR/cmake_tmp"
    unzip -q "$TOOLS_DIR/cmake.zip" -d "$TOOLS_DIR/cmake_tmp"
    mv "$TOOLS_DIR"/cmake_tmp/cmake-*/ "$TOOLS_DIR/cmake"
    rm -f "$TOOLS_DIR/cmake.zip"
    rm -rf "$TOOLS_DIR/cmake_tmp"
    if [[ ! -x "$TOOLS_DIR/cmake/bin/cmake" ]]; then
        echo "FAILED TO DOWNLOAD CMAKE"
        exit 1
    fi
    CMAKE="$TOOLS_DIR/cmake/bin/cmake"
    echo "CMake downloaded successfully."
fi

# ---- Ninja ----
NINJA_FOUND="$(command -v ninja 2>/dev/null || true)"
if [[ -z "$NINJA_FOUND" && -x "$TOOLS_DIR/ninja.exe" ]]; then
    NINJA_FOUND="$TOOLS_DIR/ninja.exe"
fi

if [[ -z "$NINJA_FOUND" ]]; then
    echo "Ninja not found — downloading to $TOOLS_DIR/ninja.exe ..."
    mkdir -p "$TOOLS_DIR"
    curl -sL "https://github.com/ninja-build/ninja/releases/latest/download/ninja-win.zip" -o "$TOOLS_DIR/ninja-win.zip"
    unzip -q "$TOOLS_DIR/ninja-win.zip" -d "$TOOLS_DIR"
    rm -f "$TOOLS_DIR/ninja-win.zip"
    if [[ ! -x "$TOOLS_DIR/ninja.exe" ]]; then
        echo "FAILED TO DOWNLOAD NINJA"
        exit 1
    fi
    echo "Ninja downloaded successfully."
fi

export PATH="$TOOLS_DIR/cmake/bin:$TOOLS_DIR:$PATH"
export CMAKE
