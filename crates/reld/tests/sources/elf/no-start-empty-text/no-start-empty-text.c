//#Config:default
//#SkipArch:ppc64le
//#RunEnabled:false
//#ExpectWarning:cannot find entry symbol
//#ExpectEntry:0
//#DiffIgnore:file-header.* #13
//#DiffIgnore:riscv_attributes.stack_align #13
//#DiffIgnore:segment.RISCV_ATTRIBUTES.* #13
//#DiffIgnore:riscv_attributes.* #13

int data = 42;
