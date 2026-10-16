//! VERSIONINFO de `tesela-scan.exe` (producto, versión y editor; sin icono, es una herramienta de consola), con
//! `winresource` (necesita `rc.exe` del Windows SDK, que acompaña a la toolchain MSVC).

fn main() {
    // Las importaciones ESTATICAS del .exe (dxgi.dll, dwmapi.dll, uxtheme.dll...) las resuelve el cargador de Windows ANTES de
    // `main`, con el orden predeterminado: la carpeta del .exe va primero y una DLL con ese nombre dejada ahi se ejecutaria
    // al arrancar. `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32) las busca solo en System32. Lo que carga el
    // programa por nombre ya en marcha lo cubre `dll_hardening` (SetDefaultDllDirectories). Ver installer/README.md.
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
