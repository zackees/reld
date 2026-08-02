//#AbstractConfig:default
//#LinkArgs:-Wl,-z,now
//#DiffIgnore:section.rodata #13
// TODO: Fix this. Note, it only shows up on openSUSE aarch64
//#DiffIgnore:rel.missing-copy-relocation.R_AARCH64_ABS64 #13

//#Config:gcc:default
//#SkipArch: ppc64le
//#LinkerDriver:g++
//#DiffIgnore:dynsym._ZTIi.section #13

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
