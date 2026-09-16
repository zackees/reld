// `__attribute__((used))` sets SHF_GNU_RETAIN, which must keep the section alive through
// --gc-sections even with no references to it.
//#CompArgs:-ffunction-sections -fdata-sections
//#LinkArgs:--gc-sections
//#RunEnabled:false
//#ExpectSym:retained_data section=".custom.retained"

__attribute__((used, section(".custom.retained"))) static int retained_data = 42;

int main(void) { return 0; }
