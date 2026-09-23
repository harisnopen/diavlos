//! Windows gives a program's main thread 1 MiB of stack unless the binary
//! asks for more; Linux and macOS give 8 MiB. Parsing the command line
//! builds every subcommand in one generated function (clap's derive), and
//! in a debug build that frame alone is close to 1 MiB. Ask Windows for the
//! same 8 MiB the others give, so the binary does not overflow before it
//! has done anything.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        if target.contains("msvc") {
            println!("cargo:rustc-link-arg-bins=/STACK:8388608");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,8388608");
        }
    }
}
