//#AbstractConfig:default
//#RequiresLinkerPlugin:true
//#Object:runtime.c
//#Object:wrap-lto-2.c:-fno-lto
//#ReferenceLinkers:
//#CompArgs:-flto
//#LinkArgs:-flto -nostdlib -z now -Wl,-wrap,foo

//#Config:gcc:default
//#LinkerDriver:gcc

//#Config:clang:default
//#Compiler:clang
//#LinkerDriver:clang
// Clang LTO bitcode requires a matching LLVMgold plugin. CI keeps its general LLD reference pin,
// so use BFD with the matching LLVMgold plugin for this LTO-specific reference.
//#ReferenceLinkers:bfd
// TODO(#13): BFD+LLVMgold emits SHT_X86_64_UNWIND while reld uses SHT_PROGBITS.
//#DiffIgnore:section.eh_frame.type

#include "../common/runtime.h"

int foo(void);
int __real_foo(void);

int __wrap_foo(void) { return __real_foo() + 32; }

void _start(void) {
  runtime_init();
  if (foo() != 42) {
    exit_syscall(100);
  }
  exit_syscall(42);
}
