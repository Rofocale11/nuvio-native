#ifndef NUVIO_ENGINE_PAYLOAD_READER_HPP
#define NUVIO_ENGINE_PAYLOAD_READER_HPP

#include <cstdint>
#include <filesystem>
#include <span>

namespace nuvio::storage {

class PayloadReader {
public:
    PayloadReader() = default;
    explicit PayloadReader(const std::filesystem::path& path, std::uint64_t tag = 0);
    ~PayloadReader();

    PayloadReader(const PayloadReader&) = delete;
    PayloadReader& operator=(const PayloadReader&) = delete;
    PayloadReader(PayloadReader&& other) noexcept;
    PayloadReader& operator=(PayloadReader&& other) noexcept;

    [[nodiscard]] bool is_open() const;
    [[nodiscard]] bool read_exact(std::uint64_t offset, std::span<char> destination) const;
    // Stremio Origin: what it was opened for (the payload can move while a stream reads it)
    [[nodiscard]] const std::filesystem::path& path() const { return path_; }
    [[nodiscard]] std::uint64_t tag() const { return tag_; }

private:
    void close();

    std::filesystem::path path_;
    std::uint64_t tag_ = 0;

#if defined(_WIN32)
    void* handle_ = nullptr;
#else
    int descriptor_ = -1;
#endif
};

}

#endif
