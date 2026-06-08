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
# Rust search core (rust-core/, crate `rcore`) — Stage 0 FFI skeleton.
#
# The core is built by cargo into a staticlib (.lib) and linked into this DLL.
# It uses the static CRT (+crt-static, see rust-core/.cargo/config.toml) so it
# matches the C++ side's /MT (Release) / /MTd (Debug) above. Only MSVC/Windows
# is wired here; the FFI core is a Windows deliverable for Stage 0/1.
#
# NOTE: building requires the Rust toolchain (rustup + the
# x86_64-pc-windows-msvc target) on PATH. The custom target below invokes
# `cargo build` at DLL-build time; if cargo is missing the build fails loudly.
# ===========================================================================
if (NOT UNIX)
	set(RCORE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../rust-core")
	set(RCORE_TARGET_TRIPLE "x86_64-pc-windows-msvc")

	# cargo's output profile dir differs between Debug and Release, and the
	# extra --release flag is only passed for non-Debug builds.
	if (CMAKE_BUILD_TYPE STREQUAL "Debug")
		set(RCORE_CARGO_PROFILE_FLAG "")
		set(RCORE_PROFILE_DIR "debug")
	else()
		set(RCORE_CARGO_PROFILE_FLAG "--release")
		set(RCORE_PROFILE_DIR "release")
	endif()

	# The produced static library. cargo names a `staticlib` crate `<name>.lib`
	# on MSVC (matching the crate name `rcore` in Cargo.toml).
	set(RCORE_LIB
		"${RCORE_DIR}/target/${RCORE_TARGET_TRIPLE}/${RCORE_PROFILE_DIR}/rcore.lib")

	# Rust sources that should trigger a relink. CONFIGURE_DEPENDS re-globs at
	# build time so editing a .rs file rebuilds the staticlib (cargo itself is
	# incremental, but CMake needs the dependency to know it must invoke cargo).
	file(GLOB_RECURSE RCORE_SOURCES CONFIGURE_DEPENDS
		"${RCORE_DIR}/src/*.rs")
	list(APPEND RCORE_SOURCES "${RCORE_DIR}/Cargo.toml")

	# Build the staticlib via cargo. The .lib is the OUTPUT (do NOT also list it
	# as BYPRODUCTS — Ninja rejects a file defined as an output twice).
	add_custom_command(
		OUTPUT "${RCORE_LIB}"
		COMMAND cargo build ${RCORE_CARGO_PROFILE_FLAG}
				--target ${RCORE_TARGET_TRIPLE}
				--manifest-path "${RCORE_DIR}/Cargo.toml"
		WORKING_DIRECTORY "${RCORE_DIR}"
		DEPENDS ${RCORE_SOURCES}
		COMMENT "Building Rust core (rcore) staticlib via cargo"
		VERBATIM
		USES_TERMINAL)

	add_custom_target(rcore_staticlib DEPENDS "${RCORE_LIB}")
	add_dependencies(${TARGET} rcore_staticlib)

	# Make RustCore.h's extern "C" declarations resolvable from the C++ sources.
	target_include_directories(${TARGET} PRIVATE src)

	# Link the Rust staticlib plus the Win32 system import libraries that a
	# minimal Rust MSVC staticlib (std runtime) pulls in:
	#   ntdll    — low-level NT calls used by std (e.g. RtlGenRandom path)
	#   userenv  — std::env / user profile lookups
	#   bcrypt   — std's RNG / hashing backend on Windows
	#   advapi32 — registry + security APIs used by std
	#   ws2_32   — sockets (already linked above for the C++ side; harmless dup)
	#   kernel32 — base Win32 (normally implicit, listed for clarity/robustness)
	# Trim this list if a future, smaller std footprint stops requiring an entry.
	target_link_libraries(${TARGET} PRIVATE
		"${RCORE_LIB}"
		ntdll userenv bcrypt advapi32 ws2_32 kernel32)
endif()
