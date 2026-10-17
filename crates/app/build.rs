//! Windows resources for `tesela.exe`:
//!  * Manifest (PLAN.md §3.9): `asInvoker` (never requests UAC at startup), long paths, PerMonitorV2 DPI, and Windows 10/11.
//!    WITHOUT `activeCodePage UTF-8`: the app uses the W APIs, and `embed-manifest` enables it by default, so it is
//!    deliberately disabled (`ActiveCodePage::System` does not write the element).
//!  * Icon (the tile logo) and VERSIONINFO (product, version, publisher), using `winresource` (requires `rc.exe` from the
//!    Windows SDK, which comes with the MSVC toolchain). The icon is DRAWN here, on every build, in `OUT_DIR`: the
//!    repository does not version images (`scripts/privacy-check.ps1` rejects them), and there are no third-party resources.
//!    `embed-manifest` writes the manifest through linker arguments, not through `rc.exe`, so the two can coexist.

use embed_manifest::manifest::{
    ActiveCodePage, DpiAwareness, ExecutionLevel, Setting, SupportedOS,
};
use embed_manifest::{embed_manifest, new_manifest};
use std::path::PathBuf;

/// Icon sizes (px): 16 to 48 for the taskbar, Start menu, and lists; 256 for large views.
const SIZES: [u32; 5] = [16, 24, 32, 48, 256];

/// Logo colors (mockup palette: accent, sky blue, and amber, suitable for color blindness).
const BLUE: [u8; 3] = [0x0F, 0x6C, 0xBD];
const SKY: [u8; 3] = [0x56, 0xB4, 0xE9];
const AMBER: [u8; 3] = [0xE6, 0x9F, 0x00];

/// Three rounded rectangles on a 256x256 canvas: (x0, y0, x1, y1, color). One tall block on the left and two
/// stacked on the right, like treemap cells.
const TILES: [(f32, f32, f32, f32, [u8; 3]); 3] = [
    (16.0, 16.0, 142.0, 240.0, BLUE),
    (152.0, 16.0, 240.0, 122.0, SKY),
    (152.0, 134.0, 240.0, 240.0, AMBER),
];
const RADIUS: f32 = 18.0;

/// Is the canvas point inside the rounded rectangle?
fn inside(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    if px < x0 || px > x1 || py < y0 || py > y1 {
        return false;
    }
    // Corners: the point is inside if its distance from the corner's center does not exceed the radius.
    let cx = px.clamp(x0 + RADIUS, x1 - RADIUS);
    let cy = py.clamp(y0 + RADIUS, y1 - RADIUS);
    let (dx, dy) = (px - cx, py - cy);
    dx * dx + dy * dy <= RADIUS * RADIUS
}

/// Draws the logo at `size` px (RGBA, straight alpha) with 8x8 supersampling for smooth edges.
fn render(size: u32) -> Vec<u8> {
    const SS: u32 = 8;
    let scale = 256.0 / size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (mut r, mut g, mut b, mut hits) = (0u32, 0u32, 0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) * scale;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) * scale;
                    for &(x0, y0, x1, y1, c) in &TILES {
                        if inside(px, py, x0, y0, x1, y1) {
                            r += c[0] as u32;
                            g += c[1] as u32;
                            b += c[2] as u32;
                            hits += 1;
                            break;
                        }
                    }
                }
            }
            if hits == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let alpha = (hits * 255 + SS * SS / 2) / (SS * SS);
                out.extend_from_slice(&[
                    (r / hits) as u8,
                    (g / hits) as u8,
                    (b / hits) as u8,
                    alpha as u8,
                ]);
            }
        }
    }
    out
}

fn png_bytes(size: u32) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut data, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("cabecera PNG");
        w.write_image_data(&render(size)).expect("datos PNG");
    }
    data
}

/// ICO container with one PNG image per size (Windows Vista and later).
fn ico_bytes() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&s| (s, png_bytes(s))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reservado
    out.extend_from_slice(&1u16.to_le_bytes()); // tipo: icono
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        // In the directory entry, 0 means 256.
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]); // ancho, alto, colores, reservado
        out.extend_from_slice(&1u16.to_le_bytes()); // planos
        out.extend_from_slice(&32u16.to_le_bytes()); // bits por pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in &images {
        out.extend_from_slice(data);
    }
    out
}

fn main() {
    // Windows resolves the executable's STATIC imports (dxgi.dll, dwmapi.dll, uxtheme.dll...) BEFORE
    // `main`, using the default order: the executable's folder comes first, so a DLL with one of those names left there
    // would run at startup. `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32) searches for them only in System32.
    // `dll_hardening` (SetDefaultDllDirectories) covers libraries loaded by name after startup. See installer/README.md.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Tesela")
            .requested_execution_level(ExecutionLevel::AsInvoker)
            .long_path_aware(Setting::Enabled)
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .active_code_page(ActiveCodePage::System)
            .supported_os(SupportedOS::Windows10..=SupportedOS::Windows10);
        embed_manifest(manifest).expect("no se pudo incrustar el manifiesto");

        let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        let ico = out_dir.join("tesela.ico");
        std::fs::write(&ico, ico_bytes()).expect("no se pudo escribir el icono");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(&ico.to_string_lossy())
            .set("ProductName", "Tesela")
            .set(
                "FileDescription",
                "Tesela - visual disk space analyzer and cleaner",
            )
            .set("CompanyName", "Tesela contributors")
            .set(
                "LegalCopyright",
                "Copyright (c) Tesela contributors. MIT OR Apache-2.0",
            )
            .set("OriginalFilename", "tesela.exe")
            .set("InternalName", "tesela");
        res.compile().expect(
            "no se pudieron incrustar el icono y la version (hace falta rc.exe del Windows SDK)",
        );
    }
    println!("cargo:rerun-if-changed=build.rs");
}
