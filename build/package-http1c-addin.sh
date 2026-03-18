#!/usr/bin/env bash
# package-http1c-addin.sh
# Packages the built DLL into a 1C add-in ZIP bundle and copies it as template.
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DLL_PATH="${1:-$REPO_ROOT/http-1c-dll/bin/libhttp1cWin.dll}"
PACKAGE_PATH="${2:-$REPO_ROOT/build/artifacts/http1c-addin.zip}"
TEMPLATE_PATH="${3:-$REPO_ROOT/http-1c-dp/http1c/Templates/http1c/Ext/Template.bin}"

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

cat > "$STAGE_DIR/MANIFEST.XML" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<bundle xmlns="http://v8.1c.ru/8.2/addin/bundle">
	<component type="native" os="Windows" arch="x86_64" path="$DLL_NAME" />
</bundle>
EOF

cp "$DLL_PATH" "$STAGE_DIR/$DLL_NAME"

rm -f "$PACKAGE_PATH" "$TEMPLATE_PATH"

# Create ZIP using available tool
if command -v zip &>/dev/null; then
    (cd "$STAGE_DIR" && zip -q "$PACKAGE_PATH" ./*)
elif command -v 7z &>/dev/null; then
    (cd "$STAGE_DIR" && 7z a -tzip "$PACKAGE_PATH" ./* > /dev/null)
else
    echo "No zip tool found (zip or 7z required)"
    exit 1
fi
cp "$PACKAGE_PATH" "$TEMPLATE_PATH"

echo "Packaged add-in bundle version $VERSION"
echo "Archive: $PACKAGE_PATH"
echo "Template updated: $TEMPLATE_PATH"
