//#Config:default
//#SkipArch: ppc64le
//#DiffIgnore:.dynamic.* #13
//#CompArgs:-C debuginfo=2
//#Shared:rdyn1.rs

//#Config:lto:default
//#RequiresLinkerPlugin:true
//#LinkerDriver:clang
// Rust LTO bitcode requires a matching LLVMgold plugin. CI keeps its general LLD reference pin,
// so use BFD with the matching LLVMgold plugin for this LTO-specific reference.
//#ReferenceLinkers:bfd
//#CompArgs:-Clinker-plugin-lto -Clink-arg=-flto -Clink-arg=-Wl,-znow
//#DiffEnabled:false

extern "C" {
    fn foo() -> i32;
    fn bar() -> i32;
    fn get_tls1() -> i32;
    fn set_tls1(value: i32);
    fn get_tls2() -> i32;
    fn set_tls2(value: i32);
}

fn main() {
    if unsafe { foo() } != 10 {
        std::process::exit(100);
    }

    if unsafe { bar() } != 18 {
        std::process::exit(101);
    }

    if unsafe { get_tls1() } != 1 {
        std::process::exit(102);
    }
    if unsafe { get_tls2() } != 2 {
        std::process::exit(103);
    }

    unsafe {
        set_tls1(88);
    }
    unsafe {
        set_tls2(55);
    }

    if unsafe { get_tls1() } != 88 {
        std::process::exit(104);
    }
    if unsafe { get_tls2() } != 55 {
        std::process::exit(105);
    }

    std::process::exit(42);
}
