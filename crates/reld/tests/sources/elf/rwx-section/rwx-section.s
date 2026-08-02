// Test that sections with writable+executable (awx) flags are placed in an RWX LOAD segment.
// Reld should warn about RWX permissions like GNU ld does.
//#Arch:x86_64
//#Mode:static
//#RunEnabled:false
//#ExpectWarningReld:has RWX \(read\+write\+execute\) permissions

.section .wtext,"awx"
.globl _start
_start:
    ret
