// Verifies undefined-symbol errors use lld's layout (reld#193): the demangled name, then
// `>>> referenced by`, the source location when there is debug info, and `<object>:(<function>)`.
// `--no-demangle` keeps the mangled name, as in lld.

//#AbstractConfig:default
//#Object:runtime.c
// GNU ld words this `undefined reference to`, so only reld is checked.
//#ReferenceLinkers:

//#Config:demangled:default
//#ExpectError:undefined symbol: foo\(int\)\n>>> referenced by [^ \n]*\.o:\(_start\)

//#Config:no-demangle:default
//#LinkArgs:--no-demangle
//#ExpectError:undefined symbol: _Z3fooi\n>>> referenced by [^ \n]*\.o:\(_start\)

//#Config:debug-info:default
//#CompArgs:-g
//#ExpectError:undefined symbol: foo\(int\)\n>>> referenced by undefined-symbol-lld-format\.c:28[^\n]*\n>>>               [^ \n]*\.o:\(_start\)

#include "../common/runtime.h"

// A C declaration bound to a C++ mangled name, so the fixture needs no C++ runtime.
void foo(int) __asm__("_Z3fooi");

void _start(void) {
  runtime_init();
  foo(1);
  exit_syscall(42);
}
