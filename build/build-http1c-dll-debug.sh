#!/usr/bin/env bash
# build-http1c-dll-debug.sh — Build DLL in debug mode
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DLL_DIR="$REPO_ROOT/http-1c-dll"

# shellcheck source=ensure-tools.sh
source "$SCRIPT_DIR/ensure-tools.sh"

# ---- Locate MSVC ----
find_vcvarsall() {
    if [[ -n "$VCVARSALL" ]]; then return 0; fi

    if [[ -n "$VSINSTALLDIR" && -f "$VSINSTALLDIR/VC/Auxiliary/Build/vcvarsall.bat" ]]; then
        VCVARSALL="$VSINSTALLDIR/VC/Auxiliary/Build/vcvarsall.bat"
        return 0
    fi

    local vcvars_path
    vcvars_path="$(command -v vcvarsall.bat 2>/dev/null || true)"
    if [[ -n "$vcvars_path" ]]; then
        VCVARSALL="$vcvars_path"
        return 0
    fi

    local vswhere=""
    vswhere="$(command -v vswhere.exe 2>/dev/null || true)"
    if [[ -z "$vswhere" ]]; then
        local pf86
        pf86="$(cmd //c 'echo %ProgramFiles(x86)%' 2>/dev/null | tr -d '\r')"
        if [[ -n "$pf86" && -f "$pf86/Microsoft Visual Studio/Installer/vswhere.exe" ]]; then
            vswhere="$pf86/Microsoft Visual Studio/Installer/vswhere.exe"
        fi
    fi

    if [[ -n "$vswhere" ]]; then
        local vs_path
        vs_path=$("$vswhere" -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>/dev/null | tr -d '\r')
        if [[ -n "$vs_path" && -f "$vs_path/VC/Auxiliary/Build/vcvarsall.bat" ]]; then
            VCVARSALL="$vs_path/VC/Auxiliary/Build/vcvarsall.bat"
            return 0
        fi
    fi

    return 1
}

VCVARSALL=""
find_vcvarsall

if [[ -z "$VCVARSALL" ]]; then
    echo "MSVC BUILD TOOLS NOT FOUND"
    echo "Install with:"
    echo '  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"'
    exit 1
fi

# Initialize MSVC environment in the current shell
_orig_path="$PATH"
_tmpbat=$(mktemp --suffix=.bat)
_tmpenv=$(mktemp)
cat > "$_tmpbat" <<ENDBAT
@call "$VCVARSALL" x64 >nul 2>&1
@if errorlevel 1 exit /b 1
@set
ENDBAT
cmd //c "$(cygpath -w "$_tmpbat")" 2>/dev/null | tr -d '\r' > "$_tmpenv"
rm -f "$_tmpbat"
while IFS='=' read -r key value; do
    [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
    export "$key=$value"
done < "$_tmpenv"
export PATH="$_orig_path:$PATH"
rm -f "$_tmpenv"

cd "$DLL_DIR"

rm -rf build_dbg
mkdir build_dbg
cd build_dbg

"$CMAKE" .. -G "Ninja" -DCMAKE_BUILD_TYPE=Debug
if [[ $? -ne 0 ]]; then
    echo "CMAKE CONFIGURE FAILED"
    exit 1
fi

"$CMAKE" --build .
if [[ $? -ne 0 ]]; then
    echo "CMAKE BUILD FAILED"
    exit 1
fi

echo ""
echo "========================================"
echo "DEBUG BUILD SUCCESSFUL"
echo "========================================"

bash "$SCRIPT_DIR/package-http1c-addin.sh"
if [[ $? -ne 0 ]]; then
    echo "ADD-IN PACKAGING FAILED"
    exit 1
fi

ls "$DLL_DIR/bin/"* 2>/dev/null
