#include "reld_mangled.hpp"

#include <iostream>
#include <string>
#include <vector>

int main() {
    using reld::consumer::abi::Accumulator;
    using reld::consumer::abi::make_formatter;
    using reld::consumer::abi::weighted_sum;

    const Accumulator accumulator;
    const int folded = accumulator.fold(std::vector<int>{2, 3, 5, 7});
    const std::string joined = accumulator.fold("mangled", "symbols");
    const int weighted = weighted_sum(4, 6);
    const auto formatter = make_formatter("reld-cxx-");

    if (folded != 17 || joined != "mangled::symbols" || weighted != 42 ||
        formatter->render(weighted) != "reld-cxx-42") {
        return 1;
    }

    std::cout << "reld-cxx-name-mangling-ok\n";
    return 0;
}
