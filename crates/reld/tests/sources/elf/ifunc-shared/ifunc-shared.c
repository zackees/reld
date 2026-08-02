//#AbstractConfig:default
//#SkipArch: ppc64le
//#RequiresGlibc:true
//#Object:compute32.c
//#CompArgs:-fPIC
//#LinkArgs:-shared -znow
//#RunDynSym:entry

//#Config:ld:default
//#DiffIgnore:.dynamic.DT_RELA* #13

//#Config:lld:default
//#ReferenceLinkers:lld
//#DiffIgnore:section.got.plt.entsize #13
//#DiffIgnore:.dynamic.DT_RELA* #13

int compute_value32(void);
int non_ifunc(void);

int entry(void) {
  if (compute_value32() != 32) {
    return 10;
  }

  if (non_ifunc() != 33) {
    return 11;
  }

  return 42;
}
