//! VERSIONINFO de `tesela-scan.exe` (producto, versión y editor; sin icono, es una herramienta de consola), con
//! `winresource` (necesita `rc.exe` del Windows SDK, que acompaña a la toolchain MSVC).

fn main() {
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
