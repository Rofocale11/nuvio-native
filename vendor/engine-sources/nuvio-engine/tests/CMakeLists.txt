add_executable(
    nuvio_engine_tests
    test_main.cpp
    byte_range_test.cpp
    ${PROJECT_SOURCE_DIR}/src/http/byte_range.cpp
    http_protocol_test.cpp
    ${PROJECT_SOURCE_DIR}/src/http/http_protocol.cpp
    loopback_server_test.cpp
    ${PROJECT_SOURCE_DIR}/src/http/loopback_server.cpp
    atomic_file_test.cpp
    ${PROJECT_SOURCE_DIR}/src/storage/atomic_file.cpp
    payload_reader_test.cpp
    ${PROJECT_SOURCE_DIR}/src/storage/payload_reader.cpp
    verified_piece_cache_test.cpp
    ${PROJECT_SOURCE_DIR}/src/cache/verified_piece_cache.cpp
    disk_cache_manager_test.cpp
    ${PROJECT_SOURCE_DIR}/src/cache/disk_cache_manager.cpp
    disk_cache_policy_integration_test.cpp
    engine_api_test.cpp
    file_selection_test.cpp
    ${PROJECT_SOURCE_DIR}/src/torrent/file_selection.cpp
    piece_scheduler_test.cpp
    ${PROJECT_SOURCE_DIR}/src/scheduler/piece_scheduler.cpp
    stream_demand_plan_test.cpp
    ${PROJECT_SOURCE_DIR}/src/scheduler/stream_demand_plan.cpp
    random_bytes_test.cpp
    ${PROJECT_SOURCE_DIR}/src/security/random_bytes.cpp
)

target_link_libraries(nuvio_engine_tests PRIVATE Nuvio::Engine)
target_compile_features(nuvio_engine_tests PRIVATE cxx_std_20)
target_include_directories(
    nuvio_engine_tests
    PRIVATE
        ${CMAKE_CURRENT_SOURCE_DIR}
        ${PROJECT_SOURCE_DIR}/src
)

if(MSVC)
    target_compile_options(nuvio_engine_tests PRIVATE /W4 /permissive-)
else()
    target_compile_options(nuvio_engine_tests PRIVATE -Wall -Wextra -Wpedantic -Wconversion -Wshadow)
endif()

add_test(NAME nuvio_engine_tests COMMAND nuvio_engine_tests)

add_executable(nuvio_engine_c_api_smoke c_api_smoke.c)
target_link_libraries(nuvio_engine_c_api_smoke PRIVATE Nuvio::Engine)
add_test(NAME nuvio_engine_c_api_smoke COMMAND nuvio_engine_c_api_smoke)

if(WIN32)
    target_link_libraries(nuvio_engine_tests PRIVATE ws2_32 bcrypt)
endif()

# Streaming window: engine + in-process web seed,
# sequential play and seek back with a small window. POSIX only.
if(NUVIO_ENGINE_ENABLE_LIBTORRENT AND NOT WIN32)
    add_executable(nuvio_engine_streaming_window_test streaming_window_test.cpp)
    target_link_libraries(
        nuvio_engine_streaming_window_test
        PRIVATE Nuvio::Engine torrent-rasterbar
    )
    target_compile_features(nuvio_engine_streaming_window_test PRIVATE cxx_std_20)
    add_test(
        NAME nuvio_engine_streaming_window_test
        COMMAND nuvio_engine_streaming_window_test
    )

    add_executable(
        nuvio_engine_streaming_window_resync_test
        streaming_window_resync_test.cpp
    )
    target_link_libraries(
        nuvio_engine_streaming_window_resync_test
        PRIVATE Nuvio::Engine torrent-rasterbar
    )
    target_compile_features(nuvio_engine_streaming_window_resync_test PRIVATE cxx_std_20)
    add_test(
        NAME nuvio_engine_streaming_window_resync_test
        COMMAND nuvio_engine_streaming_window_resync_test
    )
endif()
