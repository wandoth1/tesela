//! Conversión entre UTF-16 y WTF-8 (los nombres de Windows pueden contener sustitutos desparejados).
//!
//! Es el WTF-8 estándar y produce los mismos bytes que `OsString::from_wide(..).as_encoded_bytes()` en Windows (lo
//! comprueba un test), pero no necesita `unsafe` para volver: [`decode_to_utf16`] es total y tolera bytes inválidos
//! (los sustituye por U+FFFD), de modo que ninguna entrada puede provocar comportamiento indefinido.

/// Añade a `out` la codificación WTF-8 de `units` (UTF-16, con sustitutos desparejados permitidos).
pub fn encode_utf16(units: &[u16], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        i += 1;
        match u {
            0..=0x7F => out.push(u as u8),
            0x80..=0x7FF => {
                out.push(0xC0 | (u >> 6) as u8);
                out.push(0x80 | (u & 0x3F) as u8);
            }
            0xD800..=0xDBFF if i < units.len() && (0xDC00..=0xDFFF).contains(&units[i]) => {
                let lo = units[i];
                i += 1;
                let cp = 0x10000 + (((u as u32) - 0xD800) << 10) + ((lo as u32) - 0xDC00);
                out.push(0xF0 | (cp >> 18) as u8);
                out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
                out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                out.push(0x80 | (cp & 0x3F) as u8);
            }
            // Resto del plano básico, incluidos los sustitutos desparejados (3 bytes, como pide WTF-8).
            _ => {
                out.push(0xE0 | (u >> 12) as u8);
                out.push(0x80 | ((u >> 6) & 0x3F) as u8);
                out.push(0x80 | (u & 0x3F) as u8);
            }
        }
    }
}

/// Añade a `out` los códigos UTF-16 de unos bytes WTF-8. Total: los bytes inválidos dan U+FFFD.
pub fn decode_to_utf16(bytes: &[u8], out: &mut Vec<u16>) {
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (len, init) = match b {
            0x00..=0x7F => (1, b as u32),
            0xC2..=0xDF => (2, (b & 0x1F) as u32),
            0xE0..=0xEF => (3, (b & 0x0F) as u32),
            0xF0..=0xF4 => (4, (b & 0x07) as u32),
            _ => {
                out.push(0xFFFD);
                i += 1;
                continue;
            }
        };
        if i + len > bytes.len() || bytes[i + 1..i + len].iter().any(|c| c & 0xC0 != 0x80) {
            out.push(0xFFFD);
            i += 1;
            continue;
        }
        let cp = bytes[i + 1..i + len]
            .iter()
            .fold(init, |acc, c| (acc << 6) | (c & 0x3F) as u32);
        i += len;
        match len {
            // Una secuencia de 3 bytes que codifica menos de U+0800 es "sobrelarga" (p. ej. `E0 80 80` = U+0000): no es WTF-8
            // válido y no puede colarse como otro carácter (un NUL o un `\`).
            3 if cp < 0x800 => out.push(0xFFFD),
            1..=3 => out.push(cp as u16), // incluye sustitutos desparejados codificados en 3 bytes
            _ if (0x10000..=0x10FFFF).contains(&cp) => {
                let c = cp - 0x10000;
                out.push(0xD800 | (c >> 10) as u16);
                out.push(0xDC00 | (c & 0x3FF) as u16);
            }
            _ => out.push(0xFFFD),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generador pseudoaleatorio mínimo (SplitMix64) para no depender de `rand`.
    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn random_units(seed: u64) -> Vec<u16> {
        let mut s = seed;
        let n = (next(&mut s) % 40) as usize;
        (0..n)
            .map(|_| match next(&mut s) % 6 {
                0 => (next(&mut s) % 0x80) as u16,
                1 => (next(&mut s) % 0x800) as u16,
                2 => 0xD800 + (next(&mut s) % 0x400) as u16, // sustituto alto
                3 => 0xDC00 + (next(&mut s) % 0x400) as u16, // sustituto bajo
                _ => (next(&mut s) % 0x1_0000) as u16,
            })
            .collect()
    }

    #[test]
    fn roundtrip_con_sustitutos_desparejados() {
        for seed in 0..2000 {
            let units = random_units(seed);
            let mut bytes = Vec::new();
            encode_utf16(&units, &mut bytes);
            let mut back = Vec::new();
            decode_to_utf16(&bytes, &mut back);
            assert_eq!(units, back, "semilla {seed}");
        }
    }

    #[test]
    fn utf8_valido_coincide_con_std() {
        for s in ["", "abc", "ñandú", "日本語", "emoji 😀 fin", r"a\b"] {
            let units: Vec<u16> = s.encode_utf16().collect();
            let mut bytes = Vec::new();
            encode_utf16(&units, &mut bytes);
            assert_eq!(bytes, s.as_bytes());
        }
    }

    #[cfg(windows)]
    #[test]
    fn coincide_con_as_encoded_bytes_de_std() {
        use std::os::windows::ffi::OsStringExt;
        for seed in 0..2000 {
            let units = random_units(seed);
            let mut bytes = Vec::new();
            encode_utf16(&units, &mut bytes);
            let os = std::ffi::OsString::from_wide(&units);
            assert_eq!(bytes, os.as_encoded_bytes(), "semilla {seed}");
        }
    }

    /// GM-m10: las secuencias de 3 bytes sobrelargas (menos de U+0800) se rechazan; las válidas límite se aceptan.
    #[test]
    fn la_secuencia_de_3_bytes_sobrelarga_se_rechaza() {
        for bytes in [
            &[0xE0, 0x80, 0x80][..], // U+0000 sobrelargo
            &[0xE0, 0x80, 0xAF],     // U+002F ('/')
            &[0xE0, 0x81, 0x9C],     // U+005C ('\')
            &[0xE0, 0x9F, 0xBF],     // U+07FF sobrelargo (el máximo)
        ] {
            let mut out = Vec::new();
            decode_to_utf16(bytes, &mut out);
            assert_eq!(out, [0xFFFD], "{bytes:02X?}");
        }
        // El primer valor que SÍ necesita 3 bytes (U+0800) y los sustitutos desparejados siguen siendo válidos.
        for (bytes, unit) in [
            (&[0xE0, 0xA0, 0x80][..], 0x0800u16),
            (&[0xED, 0xA0, 0x80], 0xD800),
            (&[0xEF, 0xBF, 0xBF], 0xFFFF),
        ] {
            let mut out = Vec::new();
            decode_to_utf16(bytes, &mut out);
            assert_eq!(out, [unit], "{bytes:02X?}");
        }
    }

    #[test]
    fn bytes_invalidos_no_provocan_panico() {
        let mut out = Vec::new();
        decode_to_utf16(
            &[0xFF, 0xC0, 0x80, 0xE2, 0x82, 0xF0, 0x9F, 0x98, b'a'],
            &mut out,
        );
        assert!(out.contains(&0xFFFD));
        assert_eq!(*out.last().unwrap(), b'a' as u16);
    }
}
