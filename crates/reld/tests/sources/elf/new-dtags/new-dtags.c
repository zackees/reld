//#AbstractConfig:default
//#Object:runtime.c
//#Mode:dynamic
//#RunEnabled:false
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13

// The default, with neither flag passed: reld emits DT_RUNPATH, as ld.lld and current GNU ld do.
// This is the tag a caller gets from a bare `-rpath`, so it is pinned separately from the two
// explicit configurations below (reld#123, `rpath_tag_default_documented`). DT_RUNPATH differs
// from DT_RPATH in that it does not apply transitively to the dependencies of dependencies, and
// LD_LIBRARY_PATH takes precedence over it.
//#Config:default-dtags:default
//#LinkArgs:-shared -rpath /test/path -z now
//#ExpectDynamic:DT_RUNPATH
//#NoDynamic:DT_RPATH

//#Config:new-dtags:default
//#LinkArgs:-shared -rpath /test/path --enable-new-dtags -z now
//#ExpectDynamic:DT_RUNPATH
//#ExpectDynamic:DT_FLAGS
//#ExpectDynamic:DT_FLAGS_1
//#NoDynamic:DT_RPATH
//#NoDynamic:DT_BIND_NOW

//#Config:old-dtags:default
//#LinkArgs:-shared -rpath /test/path --disable-new-dtags -z now
//#ExpectDynamic:DT_RPATH
//#ExpectDynamic:DT_BIND_NOW
//#ExpectDynamic:DT_FLAGS_1
//#NoDynamic:DT_RUNPATH
//#NoDynamic:DT_FLAGS
//#DiffIgnore:.dynamic.DT_FLAGS_1.NOW #13
//#DiffIgnore:.dynamic.DT_RPATH #13

int foo(void);

int call_foo(void) { return foo() + 2; }
