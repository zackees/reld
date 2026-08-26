#include "reld_mangled.hpp"

namespace reld::consumer::abi {

int Accumulator::fold(int first, int second, int third, int fourth) const {
    return first + second + third + fourth;
}

long Accumulator::fold(long left, long right) const {
    return left * 11 + right * 13;
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
    explicit PrefixFormatter(int prefix) : prefix_(prefix) {}

    int render(int value) const override {
        return prefix_ + value;
    }

private:
    int prefix_;
};

} // namespace

Formatter* make_formatter(int prefix) {
    return new PrefixFormatter(prefix);
}

void destroy_formatter(Formatter* formatter) {
    delete formatter;
}

} // namespace reld::consumer::abi
