#include "reld_mangled.hpp"

extern "C" int puts(const char* text);

int main() {
    using reld::consumer::abi::Accumulator;
    using reld::consumer::abi::destroy_formatter;
    using reld::consumer::abi::make_formatter;
    using reld::consumer::abi::weighted_sum;

    const Accumulator accumulator;
    const int folded = accumulator.fold(2, 3, 5, 7);
    const long combined = accumulator.fold(3L, 5L);
    const int weighted = weighted_sum(4, 6);
    auto* formatter = make_formatter(100);

    const bool valid = folded == 17 && combined == 98 && weighted == 42 && formatter->render(weighted) == 142;
    destroy_formatter(formatter);
    if (!valid) {
        return 1;
    }

    puts("reld-cxx-name-mangling-ok");
    return 0;
}
