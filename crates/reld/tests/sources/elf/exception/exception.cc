//#AbstractConfig:default
//#LinkArgs:-Wl,-z,now
// TODO: Fix this. Note, it only shows up on openSUSE aarch64

//#Config:gcc:default
//#SkipArch: ppc64le
//#LinkerDriver:g++
//#DiffIgnore:dynsym._ZTIi.section #13 arch=x86_64

//#Config:clang:default
//#SkipArch: ppc64le
//#Compiler:clang
//#LinkerDriver:clang++

#include <iostream>

void bar() { throw 42; }

void foo() { bar(); }

int main() {
  try {
    foo();
  } catch (int myNum) {
    std::cout << myNum << std::endl;
    return myNum;
  }

  return 1;
}
