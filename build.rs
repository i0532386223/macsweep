fn main() {
    println!("cargo:rerun-if-changed=src/safety_log.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("src/safety_log.c").compile("macsweep_log");
    }
}
