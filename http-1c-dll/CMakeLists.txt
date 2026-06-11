cmake_minimum_required(VERSION 3.10)
project(http1c)
set(TARGET http1c)

set(HTTP1C_OUTPUT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/bin")

add_library(${TARGET} SHARED
		src/AddInNative.cpp
		src/AddInNative.def
		src/AddInNative.h
		src/HttpServerComponent.cpp
		src/HttpServerComponent.h
		src/ScreenCapture.cpp
		src/ScreenCapture.h
		src/stdafx.h)

target_compile_definitions(${TARGET} PRIVATE UNICODE _UNICODE
		_WIN32_WINNT=0x0601 NOMINMAX WIN32_LEAN_AND_MEAN)
target_include_directories(${TARGET} PRIVATE include)

if (UNIX)
	if (APPLE)
		set(MySuffix1 "Mac")
	else(APPLE)
		set(MySuffix1 "Lin")
	endif(APPLE)
	if (TARGET_PLATFORM_32)
		set(MySuffix2 "32")
	else()
		set(MySuffix2 "64")
	endif()
else(UNIX)
	if (NOT MSVC)
		message(FATAL_ERROR "Must be compiled with MSVC on Windows")
	endif(NOT MSVC)
	set(MyPrefix "lib")
	set(MySuffix1 "Win")
endif(UNIX)

set_target_properties( ${PROJECT_NAME} PROPERTIES
	   OUTPUT_NAME ${MyPrefix}${PROJECT_NAME}${MySuffix1}${MySuffix2} 
	   POSITION_INDEPENDENT_CODE ON 
	   CXX_STANDARD_REQUIRED ON
	   CXX_STANDARD 17
	   ARCHIVE_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}"
	   LIBRARY_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}"
	   RUNTIME_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}"
	   COMPILE_PDB_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}"
	   PDB_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}"
   )

if (UNIX)
	if (TARGET_PLATFORM_32)
		set(CMAKE_C_FLAGS "-m32 ${CMAKE_C_FLAGS}")
		set(CMAKE_CXX_FLAGS "-m32 ${CMAKE_CXX_FLAGS}")
	else(TARGET_PLATFORM_32)
		set(CMAKE_C_FLAGS "-m64 ${CMAKE_C_FLAGS}")
		set(CMAKE_CXX_FLAGS "-m64 ${CMAKE_CXX_FLAGS}")
	endif ()
	target_link_libraries(${PROJECT_NAME} -static-libstdc++)
else(UNIX)
	if (CMAKE_BUILD_TYPE STREQUAL "Debug")
		add_definitions(/MTd)
		target_compile_options(${TARGET} PRIVATE /Zi /Od)
		target_link_options(${TARGET} PRIVATE /DEBUG)
	else()
		add_definitions(/MT)
	endif()
	set(CMAKE_SUPPRESS_REGENERATION 1)
	set(CMAKE_CONFIGURATION_TYPES "Debug;Release" CACHE STRING "" FORCE)
	target_compile_definitions(${TARGET} PRIVATE _WINDOWS
			_SILENCE_CXX17_CODECVT_HEADER_DEPRECATION_WARNING)
	target_compile_options(${TARGET} PRIVATE /utf-8)
	target_link_libraries(${TARGET} PRIVATE ws2_32 gdiplus ole32 dwmapi)
endif(UNIX)

# ===========================================================================
# Rust search core (rust-core/, crate `rcore`) — runtime-loaded rcore.dll.
#
# The component does NOT link the Rust core. It loads rcore.dll at RUNTIME via
# LoadLibrary + GetProcAddress (see src/RustCore.h), so the lite component is
# PURE C++ (/MT) with ZERO cargo/Rust dependency: building libhttp1cWin.dll
# never invokes cargo. RustCore.h only needs to be on the include path.
#
# The header is in src/, so make it resolvable regardless of package flavour.
target_include_directories(${TARGET} PRIVATE src)

# ---------------------------------------------------------------------------
# Optional "full" package: also build rcore.dll (the real fastembed search core)
# and drop it next to the component. ort/onnxruntime requires the DYNAMIC CRT
# (/MD), so the cdylib is built with -crt-static REMOVED (overriding the
# +crt-static default in rust-core/.cargo/config.toml) — this is independent of
# the component's /MT and they never link together. DirectML.dll bundling is a
# separate, later step.
#
# OFF by default → the default build is the Rust-free lite component.
# ---------------------------------------------------------------------------
option(RCORE_FASTEMBED "Also build rcore.dll (real fastembed search core) for the full package" OFF)

# ===========================================================================
# Black-box smoke harness (tests/smoke) — a console exe that LoadLibrary's the
# BUILT bin/libhttp1cWin.dll exactly like 1C does (GetClassObject + stubbed 1C
# interfaces) and drives it over real HTTP. It does NOT link the DLL, so the
# CRT choice is free; we keep the DLL's flags (/MT via add_definitions above)
# for simplicity. OFF by default; CI enables it with -DHTTP1C_BUILD_TESTS=ON.
# ===========================================================================
option(HTTP1C_BUILD_TESTS "Build the http1c_smoke black-box test exe" OFF)

if (HTTP1C_BUILD_TESTS AND NOT UNIX)
	add_executable(http1c_smoke tests/smoke/main.cpp)
	target_compile_definitions(http1c_smoke PRIVATE UNICODE _UNICODE
			_WIN32_WINNT=0x0601 NOMINMAX WIN32_LEAN_AND_MEAN _WINDOWS
			_SILENCE_CXX17_CODECVT_HEADER_DEPRECATION_WARNING)
	target_include_directories(http1c_smoke PRIVATE include)
	target_compile_options(http1c_smoke PRIVATE /utf-8)
	target_link_libraries(http1c_smoke PRIVATE ws2_32)
	set_target_properties(http1c_smoke PROPERTIES
			CXX_STANDARD 17
			CXX_STANDARD_REQUIRED ON
			RUNTIME_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}"
			PDB_OUTPUT_DIRECTORY "${HTTP1C_OUTPUT_DIR}")
endif()

if (RCORE_FASTEMBED AND NOT UNIX)
	set(RCORE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../rust-core")
	set(RCORE_TARGET_TRIPLE "x86_64-pc-windows-msvc")

	# cargo always emits a cdylib under the release profile dir for this target.
	set(RCORE_DLL_SRC
		"${RCORE_DIR}/target/${RCORE_TARGET_TRIPLE}/release/rcore.dll")
	set(RCORE_DLL_OUT "${HTTP1C_OUTPUT_DIR}/rcore.dll")

	# Rust sources that should trigger a rebuild. CONFIGURE_DEPENDS re-globs at
	# build time so editing a .rs file rebuilds the cdylib (cargo itself is
	# incremental, but CMake needs the dependency to know it must invoke cargo).
	# Cargo.lock (dependency bumps) and .cargo/config.toml (CRT/link flags)
	# change the produced DLL just as much as source edits do.
	file(GLOB_RECURSE RCORE_SOURCES CONFIGURE_DEPENDS
		"${RCORE_DIR}/src/*.rs")
	list(APPEND RCORE_SOURCES
		"${RCORE_DIR}/Cargo.toml"
		"${RCORE_DIR}/Cargo.lock"
		"${RCORE_DIR}/.cargo/config.toml")

	# Build the cdylib with the dynamic CRT (ort needs /MD), then copy rcore.dll
	# into the component output dir (bin/). The copied DLL is the OUTPUT.
	add_custom_command(
		OUTPUT "${RCORE_DLL_OUT}"
		COMMAND ${CMAKE_COMMAND} -E env "RUSTFLAGS=-C target-feature=-crt-static"
				cargo build --release --features fastembed
				--target ${RCORE_TARGET_TRIPLE}
				--manifest-path "${RCORE_DIR}/Cargo.toml"
		COMMAND ${CMAKE_COMMAND} -E copy_if_different
				"${RCORE_DLL_SRC}" "${RCORE_DLL_OUT}"
		WORKING_DIRECTORY "${RCORE_DIR}"
		DEPENDS ${RCORE_SOURCES}
		COMMENT "Building Rust core (rcore.dll, fastembed) via cargo and copying to bin/"
		VERBATIM
		USES_TERMINAL)

	add_custom_target(rcore_dll DEPENDS "${RCORE_DLL_OUT}")
	add_dependencies(${TARGET} rcore_dll)
endif()
