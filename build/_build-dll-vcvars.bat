@echo off
REM One-shot DLL build inside a clean vcvars x64 context (the .sh wrapper mangles
REM MSVC PATH under MSYS bash). Configures Ninja + Release and builds.
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64
if errorlevel 1 exit /b 1

set "CMAKE=D:\GitHub\MCP-DB-Client\build\tools\cmake\bin\cmake.exe"
set "NINJA=D:\GitHub\MCP-DB-Client\build\tools\ninja.exe"

cd /d D:\GitHub\MCP-DB-Client\http-1c-dll
if exist build rmdir /s /q build
mkdir build
cd build

"%CMAKE%" .. -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_MAKE_PROGRAM="%NINJA%"
if errorlevel 1 exit /b 2

"%CMAKE%" --build .
if errorlevel 1 exit /b 3

echo BUILD_OK
