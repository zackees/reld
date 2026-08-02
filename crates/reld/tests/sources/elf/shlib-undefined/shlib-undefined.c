//#AbstractConfig:default
// We match lld's behaviour, not GNU ld's for --allow-shlib-undefined. That is, we only validate
// shared object undefined symbols when all of the shared object's direct dependencies are loaded.
//#ReferenceLinkers:lld
//#Object:runtime.c
//#Mode:dynamic
//#RunEnabled:false
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13
//#DiffIgnore:.dynamic.DT_NEEDED #13
// Ignore a few things that lld does differently.
//#DiffIgnore:section.got.plt.entsize #13

// Allow linking against shared object with undefined symbols. We don't run this because the runtime
// linker would error due to the undefined symbol.
//#Config:allow:default
//#SkipArch: ppc64le
//#Shared:shlib-undefined-2.c
//#LinkArgs:--allow-shlib-undefined -z now

// This should also succeed to link because our shared object depends on another shared object that
// we don't have loaded.
//#Config:disallow-incomplete:default
//#SkipArch: ppc64le
//#Shared:shlib-undefined-2.c
//#LinkArgs:--no-allow-shlib-undefined
// TODO(#13): Reld records NOW in DT_FLAGS_1 while LLD leaves it unset for the executable.
//#DiffIgnore:.dynamic.DT_FLAGS_1.NOW #13

// Disallow linking against shared object with undefined symbols. In this variant, the shared object
// (2) that we depend on has all of its dependencies (3) also loaded.
//#Config:disallow-complete:default
//#Shared:shlib-undefined-2.c
//#Shared:shlib-undefined-3.c
//#LinkArgs:--no-allow-shlib-undefined
//#ExpectError:def2

//#Config:shared:default
//#SkipArch: ppc64le
//#Shared:shlib-undefined-2.c
//#LinkArgs:-z now -shared
//#RunEnabled:false

#include "../common/runtime.h"

int def1(void) { return 100; }

int call_def1(void);

void _start(void) {
  runtime_init();
  exit_syscall(call_def1());
}
