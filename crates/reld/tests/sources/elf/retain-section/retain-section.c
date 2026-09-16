// `__attribute__((retain))` sets SHF_GNU_RETAIN, which must keep the section alive through
// --gc-sections even with no references to it. (C's `used` does NOT set SHF_GNU_RETAIN, unlike
// Rust's `#[used]`.)
//#LinkerDriver:gcc
//#CompArgs:-ffunction-sections -fdata-sections
//#LinkArgs:-Wl,--gc-sections
//#RunEnabled:false
//#ExpectSym:retained_data section=".custom.retained"

__attribute__((retain, section(".custom.retained"))) static int retained_data = 42;

int main(void) { return 0; }
