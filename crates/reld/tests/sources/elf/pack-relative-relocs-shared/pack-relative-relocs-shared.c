//#Config:reld-so
//#SkipArch: ppc64le
//#SoSingleLinker:reld
//#LinkerDriver:gcc
//#LinkArgs:-Wl,-z,now,-z,pack-relative-relocs
//#Shared:pack-relative-relocs-shared-1.c
//#DiffIgnore:section.rodata #13
//#DiffIgnore:section.data #13
//#DiffIgnore:rel.R_AARCH64_ADR_GOT_PAGE.R_AARCH64_ADR_GOT_PAGE #13
//#ReferenceLinkers:bfd,lld
//#DiffMatchAny:true

int foo(void);

int main() { return foo(); }
