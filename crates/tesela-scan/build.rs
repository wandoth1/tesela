//! VERSIONINFO for `tesela-scan.exe` (product, version, and publisher; no icon because it is a console tool), using
//! `winresource` (requires `rc.exe` from the Windows SDK, which comes with the MSVC toolchain).

fn main() {
    // Windows resolves the executable's STATIC imports (dxgi.dll, dwmapi.dll, uxtheme.dll...) BEFORE `main`, using the
    // default order: the .exe directory is first, so a DLL with a matching name placed there would run at startup.
    // `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32) searches for them only in System32. Loads by name after
    // startup are covered by `dll_hardening` (SetDefaultDllDirectories). See installer/README.md.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Tesela")
            .set(
                "FileDescription",
                "Tesela - read-only disk scan command line tool",
            )
            .set("CompanyName", "Tesela contributors")
            .set(
                "LegalCopyright",
                "Copyright (c) Tesela contributors. MIT OR Apache-2.0",
            )
            .set("OriginalFilename", "tesela-scan.exe")
            .set("InternalName", "tesela-scan");
        res.compile()
            .expect("no se pudo incrustar la version (hace falta rc.exe del Windows SDK)");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
