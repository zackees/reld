//#RequiresLinkerPlugin:true
//#Archive:bar.c
//#LinkerDriver:gcc
//#CompArgs:-flto
//#LinkArgs:-Wl,-znow -flto -nostdlib -O0 -shared -Wl,--whole-archive
//#RunEnabled:false
//#ExpectDynSym:foo
//#ExpectDynSym:bar

int foo(void) { return 42; }
