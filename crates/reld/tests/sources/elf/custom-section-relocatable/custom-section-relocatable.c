//#SkipArch: ppc64le
//#Object:runtime.c
//#Object:custom-section-relocatable-parts.c
//#ExpectSym:custom_alpha section=".reld.custom.alpha"
//#ExpectSym:custom_beta section=".reld.custom.beta"
//#ExpectSym:custom_section_sum section=".text"

#include "../common/runtime.h"

int custom_section_sum(void);

void _start(void) {
  runtime_init();
  if (custom_section_sum() != 42) {
    exit_syscall(1);
  }
  exit_syscall(42);
}
