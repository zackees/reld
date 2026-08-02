//#LinkerDriver:clang++
//#DiffIgnore:section.__unwind_info #13
//#DiffIgnore:section.__gcc_except_tab #13

#include <iostream>

struct Foo {
  static int foo() { return 42; }
};

int main() {
  std::cout << "hello world\n" << std::endl;
  return Foo::foo();
}
