cmake_minimum_required(VERSION 3.20)
project(BB10Music163Shell VERSION 1.0.0 LANGUAGES CXX)

option(PASSPORT_BB10 "Build the native Cascades WebView shell for BB10" ON)

set(CMAKE_CXX_STANDARD 98)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

add_compile_options(-Wall -Wextra -Werror -Wno-unused-parameter)

if(PASSPORT_BB10)
    add_definitions(-DPASSPORT_BB10)
    include(BB10Qt4)
    qt4_wrap_cpp(MOC_SOURCES src/ShellConfig.h src/LocalTlsBridge.h)
    add_executable(bb10-music163 src/main.cpp src/ShellConfig.cpp src/LocalTlsBridge.cpp ${MOC_SOURCES})
    target_include_directories(bb10-music163 PRIVATE src "${CMAKE_SOURCE_DIR}/third_party/openssl-bb10/include")
    set_target_properties(bb10-music163 PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")
    target_compile_options(bb10-music163 PRIVATE -fno-use-cxa-atexit)
    target_link_options(bb10-music163 PRIVATE -Wl,--exclude-libs,ALL)
    target_link_libraries(bb10-music163 PRIVATE
        "${CMAKE_SOURCE_DIR}/third_party/openssl-bb10/lib/libssl.a"
        "${CMAKE_SOURCE_DIR}/third_party/openssl-bb10/lib/libcrypto.a"
        bbcascades QtDeclarative QtGui QtNetwork socket QtCore m)
endif()
