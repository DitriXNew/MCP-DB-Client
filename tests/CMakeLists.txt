cmake_minimum_required(VERSION 3.10)
project(http1c_tests)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(HTTP1C_DLL_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../http-1c-dll")

add_executable(http1c_tests
    test_main.cpp
)

target_include_directories(http1c_tests PRIVATE
    ${HTTP1C_DLL_DIR}/include
    ${HTTP1C_DLL_DIR}/src
)

target_compile_definitions(http1c_tests PRIVATE 
    UNICODE _UNICODE
    _WIN32_WINNT=0x0601 NOMINMAX WIN32_LEAN_AND_MEAN
    _SILENCE_CXX17_CODECVT_HEADER_DEPRECATION_WARNING
)

if (MSVC)
    add_definitions(/MT)
    target_compile_options(http1c_tests PRIVATE /utf-8)
    target_link_libraries(http1c_tests PRIVATE ws2_32)
endif()
