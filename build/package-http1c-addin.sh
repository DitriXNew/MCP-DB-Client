#!/usr/bin/env bash
# package-http1c-addin.sh
# Packages the built DLL(s) into a 1C add-in ZIP bundle and copies it as template.
#
# Two variants (select via the VARIANT env var, default "lite"):
#   * lite — libhttp1cWin.dll alone. The search tools (search/grep/get_segment/
#            list_collections) return a structured "install RAG package" error
#            at runtime.
#   * full — libhttp1cWin.dll + rcore.dll (the real fastembed search core) +
#            DirectML.dll. rcore.dll hard-imports DirectML.dll (ort bundles the
#            DirectML execution provider), so the full bundle MUST ship it or
#            rcore.dll fails to load and the component silently degrades to lite.
#
# Usage:
#   VARIANT=lite bash build/package-http1c-addin.sh   # default
#   VARIANT=full bash build/package-http1c-addin.sh
#
# Optional positional overrides (kept for back-compat):
#   $1 — primary DLL path   (default: http-1c-dll/bin/libhttp1cWin.dll)
#   $2 — output ZIP path    (default: build/artifacts/http1c-addin[-VARIANT].zip)
#   $3 — Template.bin path   (default: the embedded 1C template)
#
# Set SKIP_TEMPLATE=1 to produce only the ZIP and NOT overwrite the embedded 1C
# Template.bin. The committed template tracks the LITE bundle (the Rust-free
# default), so the release workflow uses SKIP_TEMPLATE=1 when packaging full to
# avoid clobbering it with the heavier full bundle.
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---- Variant selection -----------------------------------------------------
VARIANT="${VARIANT:-lite}"
case "$VARIANT" in
    lite|full) ;;
    *)
        echo "Unknown VARIANT '$VARIANT' (expected 'lite' or 'full')"
        exit 1
        ;;
esac

BIN_DIR="$REPO_ROOT/http-1c-dll/bin"
DLL_PATH="${1:-$BIN_DIR/libhttp1cWin.dll}"

# Default output ZIP name carries the variant so lite/full don't clobber each
# other. The historic plain "http1c-addin.zip" name is still used for "lite" so
# existing callers/scripts keep working unchanged.
if [[ "$VARIANT" == "lite" ]]; then
    DEFAULT_PACKAGE="$REPO_ROOT/build/artifacts/http1c-addin.zip"
else
    DEFAULT_PACKAGE="$REPO_ROOT/build/artifacts/http1c-addin-$VARIANT.zip"
fi
PACKAGE_PATH="${2:-$DEFAULT_PACKAGE}"
TEMPLATE_PATH="${3:-$REPO_ROOT/http-1c-dp/http1c/Templates/http1c/Ext/Template.bin}"

# Source for DirectML.dll on the GitHub windows-latest runner / dev machines.
# Version-coupling caveat: this DirectML.dll is paired with the onnxruntime that
# ort bundled into rcore.dll. A future CPU-only onnxruntime build would drop the
# DirectML import entirely and this copy could be removed.
DIRECTML_SRC="${DIRECTML_SRC:-/c/Windows/System32/DirectML.dll}"

if [[ ! -f "$DLL_PATH" ]]; then
    echo "DLL not found: $DLL_PATH"
    exit 1
fi

VERSION_HEADER="$REPO_ROOT/http-1c-dll/version.h"
VERSION=$(grep '^#define VERSION_FULL' "$VERSION_HEADER" | awk '{print $3}')
if [[ -z "$VERSION" ]]; then
    echo "Unable to find VERSION_FULL in $VERSION_HEADER"
    exit 1
fi

DLL_NAME=$(basename "$DLL_PATH")
PACKAGE_DIR=$(dirname "$PACKAGE_PATH")
TEMPLATE_DIR=$(dirname "$TEMPLATE_PATH")

mkdir -p "$PACKAGE_DIR"
mkdir -p "$TEMPLATE_DIR"

STAGE_DIR=$(mktemp -d)
trap 'rm -rf "$STAGE_DIR"' EXIT

# ---- Assemble the list of native DLLs that go into the bundle --------------
# Every entry is "<source-path>|<name-inside-bundle>". The component DLL is
# always first; the full variant appends rcore.dll + DirectML.dll.
PAYLOAD=("$DLL_PATH|$DLL_NAME")

if [[ "$VARIANT" == "full" ]]; then
    RCORE_SRC="$BIN_DIR/rcore.dll"
    if [[ ! -f "$RCORE_SRC" ]]; then
        echo "FULL variant requested but rcore.dll not found: $RCORE_SRC"
        echo "Build it first: cmake .. -DRCORE_FASTEMBED=ON (see README / CMakeLists.txt)"
        exit 1
    fi
    PAYLOAD+=("$RCORE_SRC|rcore.dll")

    if [[ ! -f "$DIRECTML_SRC" ]]; then
        echo "FULL variant requested but DirectML.dll not found: $DIRECTML_SRC"
        echo "rcore.dll hard-imports DirectML.dll (ort's DirectML execution provider);"
        echo "without it rcore.dll fails to load and the component degrades to lite."
        echo "Set DIRECTML_SRC to its location, or install the DirectML runtime."
        exit 1
    fi
    PAYLOAD+=("$DIRECTML_SRC|DirectML.dll")
fi

# ---- Build MANIFEST.XML listing every native DLL ---------------------------
# Only the component itself is declared as a <component>; the extra runtime DLLs
# (rcore.dll, DirectML.dll) ship alongside it in the same bundle and are loaded
# by libhttp1cWin.dll at runtime (LoadLibrary), so they are <file> entries.
{
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<bundle xmlns="http://v8.1c.ru/8.2/addin/bundle">'
    echo "	<component type=\"native\" os=\"Windows\" arch=\"x86_64\" path=\"$DLL_NAME\" />"
    for entry in "${PAYLOAD[@]}"; do
        name="${entry#*|}"
        [[ "$name" == "$DLL_NAME" ]] && continue
        echo "	<file path=\"$name\" />"
    done
    echo '</bundle>'
} > "$STAGE_DIR/MANIFEST.XML"

# ---- Stage every DLL -------------------------------------------------------
for entry in "${PAYLOAD[@]}"; do
    src="${entry%|*}"
    name="${entry#*|}"
    cp "$src" "$STAGE_DIR/$name"
done

rm -f "$PACKAGE_PATH"
# Only clear the embedded template when we're actually going to rewrite it;
# SKIP_TEMPLATE=1 must leave the existing Template.bin untouched.
if [[ "${SKIP_TEMPLATE:-0}" != "1" ]]; then
    rm -f "$TEMPLATE_PATH"
fi

# Create ZIP using available tool
if command -v zip &>/dev/null; then
    (cd "$STAGE_DIR" && zip -q "$PACKAGE_PATH" ./*)
elif command -v 7z &>/dev/null; then
    (cd "$STAGE_DIR" && 7z a -tzip "$PACKAGE_PATH" ./* > /dev/null)
elif command -v powershell &>/dev/null; then
    STAGE_WIN=$(cygpath -w "$STAGE_DIR")
    PACKAGE_WIN=$(cygpath -w "$PACKAGE_PATH")
    powershell -NoProfile -Command "Compress-Archive -Path '$STAGE_WIN\\*' -DestinationPath '$PACKAGE_WIN' -Force"
else
    echo "No zip tool found (zip, 7z, or powershell required)"
    exit 1
fi
if [[ "${SKIP_TEMPLATE:-0}" == "1" ]]; then
    TEMPLATE_NOTE="skipped (SKIP_TEMPLATE=1)"
else
    cp "$PACKAGE_PATH" "$TEMPLATE_PATH"
    TEMPLATE_NOTE="$TEMPLATE_PATH"
fi

echo "Packaged add-in bundle ($VARIANT) version $VERSION"
echo "Contents:"
for entry in "${PAYLOAD[@]}"; do
    echo "  - ${entry#*|}"
done
echo "Archive: $PACKAGE_PATH"
echo "Template: $TEMPLATE_NOTE"
