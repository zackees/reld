#pragma once

#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace reld::consumer::abi {

class Accumulator {
public:
    int fold(const std::vector<int>& values) const;
    std::string fold(std::string_view left, std::string_view right) const;
};

template <typename T>
T weighted_sum(T left, T right);

extern template int weighted_sum<int>(int left, int right);

class Formatter {
public:
    virtual ~Formatter();
    virtual std::string render(int value) const = 0;
};

std::unique_ptr<Formatter> make_formatter(std::string prefix);

} // namespace reld::consumer::abi
