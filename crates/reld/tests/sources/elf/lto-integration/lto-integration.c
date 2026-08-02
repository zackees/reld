//#Config:clang
//#SkipArch: ppc64le
//#RequiresLinkerPlugin:true
//#LinkerDriver:clang
// Clang LTO bitcode requires a matching LLVMgold plugin. CI keeps its general LLD reference pin,
// so use BFD with the matching LLVMgold plugin for this LTO-specific reference.
//#ReferenceLinkers:bfd
//#Compiler:clang
//#CompArgs:-flto
//#LinkArgs:-flto -Wl,--as-needed,-znow
//#DiffIgnore:section.rodata

int main() { return 42; }
