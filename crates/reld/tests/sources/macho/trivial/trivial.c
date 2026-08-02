//#Object:runtime.c
//#ExpectSym:_main
//#TestUpdateInPlace:true
//#DiffIgnore:section.__unwind_info #13

#include "../common/runtime.h"

void main(void) { exit_syscall(42); }
