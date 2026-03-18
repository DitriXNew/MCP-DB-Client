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
	target_link_libraries(${TARGET} PRIVATE ws2_32)
endif(UNIX)
