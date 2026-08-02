// Checks what we do if we try to link LTO inputs when no plugin is supplied.

//#AbstractConfig:default
//#ReferenceLinkers:
//#ExpectError:linker plugin was not supplied
//#CompArgs:-flto
// This fixture deliberately drives reld directly without a plugin. Compiling the LTO object is
// the capability check; requiring a linker driver plugin would contradict the behavior under test.

//#Config:gcc:default
//#Compiler:gcc

//#Config:clang:default
//#Compiler:clang

void _start(void) {}
