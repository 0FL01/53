cmake_minimum_required(VERSION 3.18)
project(dmsg_slipstream C CXX)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(PICOQUIC_FETCH_PTLS ON CACHE BOOL "" FORCE)
set(BUILD_DEMO OFF CACHE BOOL "" FORCE)
set(BUILD_HTTP OFF CACHE BOOL "" FORCE)
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)
set(BUILD_LOGLIB OFF CACHE BOOL "" FORCE)
set(BUILD_LOGREADER OFF CACHE BOOL "" FORCE)
set(DISABLE_DEBUG_PRINTF ON CACHE BOOL "" FORCE)
set(PTLS_WITH_FUSION OFF CACHE BOOL "" FORCE)
set(WITH_FUSION OFF CACHE BOOL "" FORCE)
set(WITH_DTRACE OFF CACHE BOOL "" FORCE)
# pkg-config caches do not account for PKG_CONFIG_LIBDIR changing. Keep all
# dependency discovery confined to the OpenSSL built for this Cargo target.
unset(__pkg_config_checked__OPENSSL CACHE)
unset(__pkg_config_checked_BROTLI_DEC CACHE)
unset(__pkg_config_checked_BROTLI_ENC CACHE)
add_subdirectory(${SLIPSTREAM_SOURCE}/subprojects/picoquic picoquic)
# Picotls defines its CLI/tests unconditionally; the library boundary needs only
# its production static targets. Do not build tool executables for Android.
get_property(ptls_targets DIRECTORY ${picotls_SOURCE_DIR} PROPERTY BUILDSYSTEM_TARGETS)
foreach(t IN LISTS ptls_targets)
    get_target_property(kind ${t} TYPE)
    if(kind STREQUAL "EXECUTABLE")
        set_target_properties(${t} PROPERTIES EXCLUDE_FROM_ALL TRUE)
    endif()
endforeach()
set(S ${SLIPSTREAM_SOURCE})
add_library(dmsg_slipstream STATIC
    embed.c
    ${S}/src/slipstream_client_runtime.c ${S}/src/slipstream_client.c
    ${S}/src/slipstream_flow.c ${S}/src/slipstream_pin.c
    ${S}/src/slipstream_bridge.c ${S}/src/slipstream_sockloop.c
    ${S}/src/slipstream_utils.c ${S}/src/slipstream_inline_dots.c
    ${S}/extern/lua-resty-base-encoding/base32.c
    ${S}/extern/SPCDNS/src/codec.c ${S}/extern/SPCDNS/src/mappings.c
    ${S}/extern/SPCDNS/src/netsimple.c ${S}/extern/SPCDNS/src/output.c)
target_compile_definitions(dmsg_slipstream PRIVATE DMSG_EMBEDDED DISABLE_DEBUG_PRINTF)
target_compile_options(dmsg_slipstream PRIVATE -Wall -Wextra)
target_include_directories(dmsg_slipstream PRIVATE
    . ${S}/include ${S}/extern ${S}/extern/SPCDNS/src
    ${S}/extern/lua-resty-base-encoding
    ${S}/subprojects/picoquic/picoquic
    ${picotls_SOURCE_DIR}/include ${OPENSSL_INCLUDE_DIR})
target_link_libraries(dmsg_slipstream PRIVATE picoquic-core)
install(TARGETS dmsg_slipstream picoquic-core picotls-core picotls-minicrypto
    picotls-openssl ARCHIVE DESTINATION lib)
