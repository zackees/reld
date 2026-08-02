// A test for #1472.

//#Object:runtime.c
//#CompArgs:-fno-PIC
//#Mode:dynamic
//#Shared:force-dynamic-linking.c
//#DiffIgnore:.dynamic.DT_NEEDED #13
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13
//#DiffIgnore:rel.undefined-weak.dynamic.R_X86_64_GLOB_DAT #13

#include "../common/runtime.h"

#define WEAK __attribute__((weak))

int WEAK foo(void);

void _start(void) {
  runtime_init();
  if (foo) {
    exit_syscall(foo());
  }
  exit_syscall(42);
}
