/* For some reason, GAS on riscv64 does not support '//' comments.
//#LinkArgs:-shared -z now
//#RunEnabled:false
//#ExpectSym:abs_sym address=0xCAFECAFE
//#ExpectDynSym:abs_sym address=0xCAFECAFE

// TODO: checkout those differences later
//#DiffIgnore:segment.RISCV_ATTRIBUTES.alignment #13
//#DiffIgnore:segment.RISCV_ATTRIBUTES.flags #13
//#DiffIgnore:riscv_attributes..riscv.attributes #13
//#DiffIgnore:riscv_attributes.arch #13
//#DiffIgnore:riscv_attributes.stack_align #13
*/

.global abs_sym
.set abs_sym, 0xCAFECAFE
