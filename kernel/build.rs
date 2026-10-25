fn main() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("linker.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    if std::env::var("TARGET").as_deref() == Ok("x86_64-unknown-none") {
        println!("cargo:rustc-link-arg=-T{}", script.display());
    }
}
