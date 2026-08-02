//#AbstractConfig:default
//#LinkerDriver:gcc
//#LinkArgs:-Wl,-z,now
//#ExpectSym:main

//#Config:gcc:default
//#SkipArch: ppc64le
// TODO(#13): Reld omits the empty .rodata section emitted by BFD.
//#DiffIgnore:section.rodata #13

//#Config:gcc-static:default
//#SkipArch: ppc64le
//#LinkArgs:-static -Wl,--gc-sections
//#DiffIgnore:section.rela.plt.link #13
// RISC-V BFD keeps the symbol in .dynsym.
//#DiffIgnore:section.rela.dyn #13 arch=riscv64

//#Config:gcc-static-pie-no-relax:default
//#CompArgs:-fPIE
//#LinkArgs:-static-pie -Wl,--gc-sections -Wl,--no-relax
//#DiffEnabled:false
//#ReferenceLinkers:
// TODO: #874
//#SkipArch: riscv64,ppc64le

//#Config:clang-static:default
//#SkipArch: ppc64le
//#Compiler:clang
//#LinkArgs:-static
//#DiffIgnore:section.rela.plt.link #13

//#Config:clang-static-pie-no-relax:default
//#Compiler:clang
//#CompArgs:-fPIE
//#LinkArgs:-static-pie -Wl,--gc-sections -Wl,--no-relax
//#DiffEnabled:false
//#ReferenceLinkers:
// For some reason, both linkers cannot find: `rcrt1.o`
//#SkipArch: riscv64,ppc64le

//#Config:clang:default
//#SkipArch: ppc64le
//#Compiler: clang
// TODO(#13): Reld omits the empty .rodata section emitted by BFD.
//#DiffIgnore:section.rodata #13

//#Config:gcc-indirect-external:default
//#CompArgs:-fPIE -mno-direct-extern-access
//#RequiresCompilerFlags:-mno-direct-extern-access
// TODO(#13): Reld omits the empty .rodata section emitted by BFD.
//#DiffIgnore:section.rodata #13

int main() { return 42; }
