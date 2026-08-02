//#SkipArch: ppc64le
//#LinkArgs:-z now -Bshareable --exclude-libs somelib
//#Mode:dynamic
//#RunEnabled:false
//#Archive:exclude-libs-single-1.c
//#ExpectDynSym:foo
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13

extern int foo(void);

// Use foo so that it's not garbage collected.
int call_foo(void) { return foo() + 2; }
