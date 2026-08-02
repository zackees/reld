//#LinkArgs:-z now -Bshareable --exclude-libs ALL
//#Mode:dynamic
//#RunEnabled:false
//#Archive:exclude-libs-all-1.c
// This symbol shouldn't end up in .dynsym. reld-diff checks this.
int foo(void);

int call_foo(void) {
  // This reference to foo should be optimised by the linker, since the symbol is made hidden, so we
  // know it cannot be overridden.
  return foo() + 2;
}
