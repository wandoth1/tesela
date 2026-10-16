//! Recursos de Windows de `tesela.exe`:
//!  * Manifiesto (PLAN.md §3.9): `asInvoker` (nunca pide UAC al arrancar), rutas largas, DPI PerMonitorV2 y Windows 10/11.
//!    SIN `activeCodePage UTF-8`: la app usa las APIs W y el valor por defecto de `embed-manifest` lo activaría, así
//!    que se desactiva a propósito (`ActiveCodePage::System` no escribe el elemento).
//!  * Icono (el logotipo de mosaico) y VERSIONINFO (producto, versión, editor), con `winresource` (necesita `rc.exe` del
//!    Windows SDK, que acompaña a la toolchain MSVC). El icono se DIBUJA aquí, en cada compilación, en `OUT_DIR`: el
//!    repositorio no versiona imágenes (`scripts/privacy-check.ps1` las rechaza) y no hay recursos de terceros. El
//!    manifiesto lo escribe `embed-manifest` por argumentos del enlazador, no por `rc.exe`: los dos conviven.

use embed_manifest::manifest::{
    ActiveCodePage, DpiAwareness, ExecutionLevel, Setting, SupportedOS,
};
use embed_manifest::{embed_manifest, new_manifest};
use std::path::PathBuf;

/// Tamaños del icono (px): 16 a 48 para la barra de tareas, el menú Inicio y las listas; 256 para las vistas grandes.
const SIZES: [u32; 5] = [16, 24, 32, 48, 256];

/// Colores del logotipo (paleta de la maqueta: acento, celeste y ámbar, aptos para daltonismo).
const BLUE: [u8; 3] = [0x0F, 0x6C, 0xBD];
const SKY: [u8; 3] = [0x56, 0xB4, 0xE9];
const AMBER: [u8; 3] = [0xE6, 0x9F, 0x00];

/// Tres rectángulos redondeados en un lienzo de 256x256: (x0, y0, x1, y1, color). Un bloque alto a la izquierda y dos
/// apilados a la derecha, como las celdas de un treemap.
const TILES: [(f32, f32, f32, f32, [u8; 3]); 3] = [
    (16.0, 16.0, 142.0, 240.0, BLUE),
    (152.0, 16.0, 240.0, 122.0, SKY),
    (152.0, 134.0, 240.0, 240.0, AMBER),
];
const RADIUS: f32 = 18.0;

/// ¿Está el punto del lienzo dentro del rectángulo redondeado?
fn inside(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    if px < x0 || px > x1 || py < y0 || py > y1 {
        return false;
    }
    // Esquinas: el punto está dentro si su distancia al centro de la esquina no pasa del radio.
    let cx = px.clamp(x0 + RADIUS, x1 - RADIUS);
    let cy = py.clamp(y0 + RADIUS, y1 - RADIUS);
    let (dx, dy) = (px - cx, py - cy);
    dx * dx + dy * dy <= RADIUS * RADIUS
}

/// Dibuja el logotipo a `size` px (RGBA, alfa recta) con supersampling 8x8 para bordes suaves.
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

/// Contenedor ICO con una imagen PNG por tamaño (Windows Vista y posteriores).
fn ico_bytes() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&s| (s, png_bytes(s))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reservado
    out.extend_from_slice(&1u16.to_le_bytes()); // tipo: icono
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        // En el directorio, 0 significa 256.
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
    // Las importaciones ESTATICAS del .exe (dxgi.dll, dwmapi.dll, uxtheme.dll...) las resuelve el cargador de Windows ANTES de
    // `main`, con el orden predeterminado: la carpeta del .exe va primero y una DLL con ese nombre dejada ahi se ejecutaria
    // al arrancar. `/DEPENDENTLOADFLAG:0x800` (LOAD_LIBRARY_SEARCH_SYSTEM32) las busca solo en System32. Lo que carga el
    // programa por nombre ya en marcha lo cubre `dll_hardening` (SetDefaultDllDirectories). Ver installer/README.md.
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
