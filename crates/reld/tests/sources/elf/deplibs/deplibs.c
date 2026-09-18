// reld#180: an object whose .deplibs (SHT_LLVM_DEPENDENT_LIBRARIES) section names `m` must be
// linked against libm without -lm on the command line, as ld.lld does. clang emits .deplibs for
// `#pragma comment(lib, ...)` on ELF; GCC does not, hence Compiler:clang. GNU ld does not honour
// .deplibs, so lld is the only reference linker.

//#AbstractConfig:default
//#Compiler:clang
//#LinkerDriver:clang
//#RequiresGlibc:true
//#CompArgs:-fno-builtin

//#Config:deplibs-are-linked:default
//#ReferenceLinkers:lld
//#NoSection:.deplibs
//#DiffIgnore:section.got.plt.entsize #13
//#DiffIgnore:section.gnu.version_r.alignment #13

//#Config:deplibs-missing:default
//#Object:deplibs-missing.c
//#ReferenceLinkers:
//#ExpectError:unable to find library from dependent library specifier: reld_deplibs_does_not_exist

#pragma comment(lib, "m")

double cbrt(double);

int main(void) {
  volatile double in = 27.0;
  double out = cbrt(in);
  return out > 2.999 && out < 3.001 ? 42 : 1;
}
