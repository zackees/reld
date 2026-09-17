// A copy relocation of a symbol the shared object kept read-only must land inside PT_GNU_RELRO,
// otherwise the loader cannot re-protect it and a `const` stays writable for the whole process.
// GNU ld puts it in `.data.rel.ro`; lld uses `.bss.rel.ro`. Reference linkers are disabled here
// because those two spellings differ, so this pins reld's own placement (reld#123 Phase 2).
//#SkipArch: ppc64le
//#Object:runtime.c
//#ReferenceLinkers:
//#Mode:dynamic
//#Shared:copy-relocation-relro-lib.c
//#ExpectSym:ro_value section=".data.rel.ro"
//#ExpectSym:rw_value section=".bss"

#include "../common/runtime.h"

extern const int ro_value;
extern int rw_value;

void _start(void) {
  runtime_init();

  if (ro_value != 7) {
    exit_syscall(50);
  }
  if (rw_value != 8) {
    exit_syscall(51);
  }

  exit_syscall(42);
}
