// One notable scenario that this test tests is having a non-weak undefined symbol (baz) in a shared
// object and having that symbol be defined by an archive entry that we don't load.

//#Config:default
//#SkipArch: ppc64le
//#LinkArgs:-shared -z now
//#Mode:dynamic
//#RunDynSym:foo
//#Shared:shared-s1.c
//#Archive:shared-a1.c,shared-a2.c
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13
//#DiffIgnore:.dynamic.DT_NEEDED #13
//#ExpectDynSym:foo
//#ExpectDynSym:call_bar1

//#Config:symbolic:default
//#SkipArch: ppc64le
//#LinkArgs:-shared -z now -Bsymbolic
//#DiffIgnore:.dynamic.DT_FLAGS.SYMBOLIC #13
//#DiffIgnore:.dynamic.DT_SYMBOLIC #13
//#DiffIgnore:section.got #13
//#DiffIgnore:rel.R_X86_64_PC32.R_X86_64_PLT32 #13
//#ExpectDynamic:DT_FLAGS

//#Config:symbolic-functions:default
//#SkipArch: ppc64le
//#LinkArgs:-shared -z now -Bsymbolic-functions

//#Config:nosymbolic:default
//#SkipArch: ppc64le
//#LinkArgs:-shared -z now -Bno-symbolic

//#Config:symbolic-non-weak:default
//#SkipArch: ppc64le
//#LinkArgs:-shared -z now -Bsymbolic-non-weak
//#ReferenceLinkers:lld
//#DiffIgnore:section.got.plt.entsize #13
//#DiffIgnore:section.relro_padding #13

//#Config:symbolic-non-weak-functions:default
//#SkipArch: ppc64le
//#LinkArgs:-shared -z now -Bsymbolic-non-weak-functions
//#ReferenceLinkers:lld
//#DiffIgnore:section.relro_padding #13
//#DiffIgnore:section.got.plt.entsize #13
//#DiffIgnore:dynsym.baz.section #13

int bar1(void);
int bar2(void);

int foo(void) { return bar1() + bar2(); }

int call_bar1(void) { return bar1(); }
