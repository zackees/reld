#pragma once

namespace reld::consumer::abi {

class Accumulator {
public:
    int fold(int first, int second, int third, int fourth) const;
    long fold(long left, long right) const;
};

template <typename T>
T weighted_sum(T left, T right);

extern template int weighted_sum<int>(int left, int right);

class Formatter {
public:
    virtual ~Formatter();
    virtual int render(int value) const = 0;
};

Formatter* make_formatter(int prefix);
void destroy_formatter(Formatter* formatter);

} // namespace reld::consumer::abi
