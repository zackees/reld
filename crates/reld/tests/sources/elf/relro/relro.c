//#AbstractConfig:default
// Create a .got.plt section to force ld to include a PT_GNU_RELRO program header
//#Shared:runtime.c
//#Mode:dynamic
//#DiffIgnore:section.got #13
//#DiffIgnore:.dynamic.DT_NEEDED #13
//#DiffIgnore:.dynamic.DT_FLAGS_1.NOW #13
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13

//#Config:enabled:default
//#SkipArch: ppc64le
//#LinkArgs:-z relro
//#ExpectProgramHeader:GNU_RELRO flags=R,sections=[*]

//#Config:disabled:default
//#SkipArch: ppc64le
//#LinkArgs:-z norelro
//#NoProgramHeader:GNU_RELRO
//#DoesNotContain:relro_padding

#include "../common/runtime.h"

void _start() {
  runtime_init();
  exit_syscall(42);
}
