// A shared object that references a preemptable external symbol by absolute address (compiled
// without PIC) carries a text relocation and must emit `DT_TEXTREL`. `-z notext` is satisfied by
// construction: the native engine never errors on text relocations, it just records `DT_TEXTREL`.
//#CompArgs:-fno-pic
//#LinkArgs:-shared -z notext
//#RunEnabled:false
//#ReferenceLinkers:bfd,lld
//#ExpectDynamic:DT_TEXTREL

extern int ext_global;

int *get_ext(void) {
    return &ext_global;
}
