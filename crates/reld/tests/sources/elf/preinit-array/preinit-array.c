//#Object:preinit-array.s
//#Shared:runtime.c
// We're linking different .so files, so this is expected.
//#DiffIgnore:.dynamic.DT_NEEDED #13
//#DiffIgnore:segment.LOAD.RW.alignment #13
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13
//#Arch: x86_64
//#RequiresGlibc:true
//#Mode:dynamic

#include "../common/runtime.h"

int exit_code;

void preinit() { exit_code = 42; }

void _start(void) {
  runtime_init();
  exit_syscall(exit_code);
}
