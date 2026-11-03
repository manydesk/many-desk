// The build number (MANYDESK_BUILD, set by scripts/package-native.sh as yyyymmdd.hhmm;
// "dev" otherwise) goes into the binary next to the version. Windows: icon and version
// info in the exe (mingw windres when cross-compiling).
fn main() {
    println!("cargo:rerun-if-env-changed=MANYDESK_BUILD");
    let build = std::env::var("MANYDESK_BUILD").ok().filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "dev".into());
    println!("cargo:rustc-env=MANYDESK_BUILD={build}");
    // `mobile`: Android and iOS — no desktop capture, file dialogs or clipboard crates.
    println!("cargo::rustc-check-cfg=cfg(mobile)");
    if matches!(std::env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("android" | "ios")) {
        println!("cargo:rustc-cfg=mobile");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        let version = format!("{} (build {build})", std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
        res.set_icon("assets/icon.ico")
            .set("ProductName", "ManyDesk")
            .set("FileDescription", "ManyDesk remote desktop")
            .set("ProductVersion", &version);
        if let Err(e) = res.compile() {
            println!("cargo:warning=no Windows resources: {e}");
        }
    }
}
