fn main() {
    println!("cargo:rerun-if-changed=src/safety_log.c");
    println!("cargo:rerun-if-changed=src/cloud_file.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/cloud_file.m")
            .flag("-fobjc-arc")
            .compile("macsweep_cloud");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=Foundation");
        cc::Build::new().file("src/safety_log.c").compile("macsweep_log");
    }
}
