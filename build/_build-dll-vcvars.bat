@echo off
REM One-shot DLL build inside a clean vcvars x64 context (the .sh wrapper mangles
REM MSVC PATH under MSYS bash). Configures Ninja + Release and builds.
REM Paths derive from this script's location; override via the VCVARSALL / CMAKE /
REM NINJA environment variables when the defaults don't match the machine.
setlocal

set "REPO_ROOT=%~dp0.."

if not defined VCVARSALL set "VCVARSALL=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat"
call "%VCVARSALL%" x64 || exit /b 1

if not defined CMAKE set "CMAKE=%REPO_ROOT%\build\tools\cmake\bin\cmake.exe"
if not defined NINJA set "NINJA=%REPO_ROOT%\build\tools\ninja.exe"

cd /d "%REPO_ROOT%\http-1c-dll" || exit /b 1
if exist build rmdir /s /q build
mkdir build
cd build || exit /b 1

"%CMAKE%" .. -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_MAKE_PROGRAM="%NINJA%" || exit /b 2

"%CMAKE%" --build . || exit /b 3

echo BUILD_OK
