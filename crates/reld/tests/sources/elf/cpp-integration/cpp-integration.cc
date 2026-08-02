//#AbstractConfig:default
// On aarch64, GNU ld puts the copy relocation for this symbol in .data.rel.ro rather than .bss.
//#DiffIgnore:dynsym.__stack_chk_guard.section #13 arch=aarch64
//#Object:cpp-integration-2.cc
//#DiffMatchAny:true

//#Config:pie:default
//#SkipArch: ppc64le
//#CompArgs:-fpie -fmerge-constants
//#LinkerDriver:g++
//#LinkArgs:-pie -Wl,-z,now
//#ReferenceLinkers:bfd,lld
//#DiffIgnore:section.rodata.alignment #13

//#Config:no-pie:default
//#SkipArch: ppc64le
//#CompArgs:-fno-pie -fmerge-constants
//#LinkerDriver:g++
//#LinkArgs:-no-pie -Wl,-z,now
//#ReferenceLinkers:bfd,lld
//#DiffIgnore:section.rodata.alignment #13
// TODO(#13): Reld omits the empty .data section emitted by both reference linkers.
//#DiffIgnore:section.data #13

//#Config:static-no-relax:default
//#CompArgs:-fmerge-constants
//#LinkerDriver:g++
//#LinkArgs:-static -Wl,-z,now,-no-relax
//#DiffIgnore:section.rela.plt.link #13
// Reld uses similar order as LLD, which is different from GNU ld.
//#DiffIgnore:init_array #13
// TODO: Missing `endbr64` relaxations.
//#DiffIgnore:rel.match_failed.R_X86_64_PLT32 #13
//#DiffIgnore:literal-byte-mismatch #13
// TODO: Some conditions for required relaxations are wrong.
//#DiffIgnore:rel.extra-opt.R_X86_64_REX_GOTPCRELX.RexCmpIndirectToAbsolute* #13
//#DiffIgnore:rel.extra-opt.R_X86_64_REX_GOTPCRELX.RexMovIndirectToAbsolute* #13
//#DiffIgnore:rel.missing-opt.R_X86_64_GOTTPOFF.RexMovIndirectToAbsolute* #13
//#Arch: x86_64

//#Config:clang-pie:default
//#SkipArch: ppc64le
//#CompArgs:-fpie
//#Compiler:clang
//#LinkerDriver:clang++
//#LinkArgs:-pie -Wl,-z,now
//#ReferenceLinkers:bfd,lld
//#DiffIgnore:section.rodata.alignment #13

//#Config:model-large:default
//#CompArgs:-mcmodel=large
//#LinkerDriver:g++
//#LinkArgs:-Wl,-z,now
//#ReferenceLinkers:bfd,lld
//#DiffIgnore:section.rodata.alignment #13
// TODO: Ubuntu: cc1plus: sorry, unimplemented: code model 'large' with '-fPIC'
//#Arch: x86_64

//#Config:clang-model-large:default
//#Compiler:clang
//#CompArgs:-mcmodel=large
//#LinkerDriver:clang++
//#LinkArgs:-Wl,-z,now
//#ReferenceLinkers:bfd,lld
// TODO(#13): Reld omits the empty .rodata section emitted by both reference linkers.
//#DiffIgnore:section.rodata #13
//#Arch: x86_64

//#Config:clang-crel:default
//#SkipArch: ppc64le
//#Compiler:clang
//#CompArgs: -Wa,--crel,--allow-experimental-crel
//#LinkerDriver:clang++
//#RequiresCompilerFlags:-Wa,--crel,--allow-experimental-crel
//#DiffEnabled:false
//#ReferenceLinkers:

#include <iostream>
#include <string>

const char* colon();
const char* char_c();
const char* char_d();

int main() {
  std::string foo;
  foo += "aaa";
  foo += colon();
  foo += "b";
  foo += ":";
  foo += char_c();
  foo += ":";
  foo += "d";
  if (foo != "aaa:b:c:d") {
    std::cout << foo << std::endl;
    return 10;
  }
  return 42;
}
