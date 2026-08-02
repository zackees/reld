//#Mode:dynamic
//#RunEnabled:false
//#ReferenceLinkers:lld
//#LinkArgs:-shared -z now -T ./linker-script-discard.ld
//#DiffIgnore:section.got #13
//#DiffIgnore:segment.LOAD.RX.alignment #13
//#DiffIgnore:segment.LOAD.RWX.alignment #13
// Reld does not emit the `.eh_frame` section as all code sections are discarded, but lld still
// emits the CIE.
//#DiffIgnore:section.eh_frame #13
//#DoesNotContain:/DISCARD/
//#DoesNotContain:.text
//#NoSym:foo

int foo() { return 0; }
