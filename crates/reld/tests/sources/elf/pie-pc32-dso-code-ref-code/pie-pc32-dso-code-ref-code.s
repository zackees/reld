// Scenario: code references DSO function via PLT call - valid in PIE
//#Arch:x86_64
//#Mode:dynamic
//#Shared:pie-pc32-dso-shared-fn.s
//#SoSingleLinker:reld
//#LinkArgs:-pie
//#RunEnabled:false
//#ReferenceLinkers:lld
//#DiffIgnore:.dynamic.DT_FLAGS_1.NOW #13
//#DiffIgnore:.dynamic.DT_RELA #13
//#DiffIgnore:.dynamic.DT_RELAENT #13
//#DiffIgnore:section.got.plt.entsize #13
.global _start
_start:
    call zed_fn
    ret
