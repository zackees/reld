#include "reld_mangled.hpp"

#include <numeric>
#include <utility>

namespace reld::consumer::abi {

int Accumulator::fold(const std::vector<int>& values) const {
    return std::accumulate(values.begin(), values.end(), 0);
}

std::string Accumulator::fold(std::string_view left, std::string_view right) const {
    return std::string(left) + "::" + std::string(right);
}

template <typename T>
T weighted_sum(T left, T right) {
    return left * 3 + right * 5;
}

template int weighted_sum<int>(int left, int right);

Formatter::~Formatter() = default;

namespace {

class PrefixFormatter final : public Formatter {
public:
    explicit PrefixFormatter(std::string prefix) : prefix_(std::move(prefix)) {}

    std::string render(int value) const override {
        return prefix_ + std::to_string(value);
    }

private:
    std::string prefix_;
};

} // namespace

std::unique_ptr<Formatter> make_formatter(std::string prefix) {
    return std::make_unique<PrefixFormatter>(std::move(prefix));
}

} // namespace reld::consumer::abi
