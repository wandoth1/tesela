//! Squarified treemap (Bruls, Huizing y van Wijk, 2000).
//!
//! Como los hijos llegan ya ordenados por tamaño descendente (lo garantiza el `TreeBuilder`), el algoritmo es lineal:
//! cada valor se examina una vez al crecer la fila y otra al colocarla.

use crate::geom::Rect;

/// Reparte `values` (en el orden dado; lo ideal es descendente) dentro de `rect`, de modo que el área de cada
/// rectángulo sea `value * rect.area / total`. Añade un rectángulo por valor a `out`, en el mismo orden.
///
/// `total` puede ser MAYOR que la suma de `values`: lo que sobra queda como hueco al final (así se deja sin dibujar
/// la cola de elementos diminutos sin cambiar la escala del resto). Si la suma es `total`, los rectángulos cubren
/// `rect` exactamente (el último de cada fila se ajusta al borde para que no queden rendijas por el redondeo).
///
/// Los valores deben ser positivos: uno nulo o negativo recibe un rectángulo vacío.
pub fn squarify(values: &[f64], total: f64, rect: Rect, out: &mut Vec<Rect>) {
    let start = out.len();
    let (x0, y0, w0, h0) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let area = w0 * h0;
    if values.is_empty() {
        return;
    }
    if !pos(total) || !pos(area) {
        out.extend(values.iter().map(|_| Rect::new(rect.x, rect.y, 0.0, 0.0)));
        return;
    }
    let sum: f64 = values.iter().filter(|v| **v > 0.0).sum();
    let exact = (total - sum).abs() <= 1e-9 * total;
    let scale = area / total;
    let (mut x, mut y, mut w, mut h) = (x0, y0, w0, h0);
    let n = values.len();
    let mut i = 0;
    while i < n {
        if !pos(values[i]) {
            out.push(Rect::new(x as f32, y as f32, 0.0, 0.0));
            i += 1;
            continue;
        }
        let short = w.min(h);
        if !pos(short) {
            // Sin sitio (por el redondeo): lo que queda recibe rectángulos vacíos.
            out.extend((i..n).map(|_| Rect::new(x as f32, y as f32, 0.0, 0.0)));
            break;
        }
        // Se crece la fila mientras la peor relación de aspecto no empeore.
        let mut j = i;
        let (mut row_sum, mut worst_prev) = (0.0f64, f64::INFINITY);
        let (mut rmax, mut rmin) = (0.0f64, f64::INFINITY);
        while j < n && values[j] > 0.0 {
            let a = values[j] * scale;
            let s2 = row_sum + a;
            let (mx, mn) = (rmax.max(a), rmin.min(a));
            let worst = ((short * short * mx) / (s2 * s2)).max((s2 * s2) / (short * short * mn));
            if j > i && worst > worst_prev {
                break;
            }
            row_sum = s2;
            rmax = mx;
            rmin = mn;
            worst_prev = worst;
            j += 1;
        }
        let last_row = exact && (j == n || values[j] <= 0.0);
        if w >= h {
            // Fila vertical pegada al borde izquierdo: sus elementos se apilan de arriba abajo.
            let thick = if last_row { w } else { (row_sum / h).min(w) };
            let mut cy = y;
            for (k, &v) in values.iter().enumerate().take(j).skip(i) {
                let a = v * scale;
                let len = if k + 1 == j && (last_row || exact) {
                    (y + h) - cy
                } else {
                    a / thick
                };
                out.push(Rect::new(
                    x as f32,
                    cy as f32,
                    thick as f32,
                    len.max(0.0) as f32,
                ));
                cy += len;
            }
            x += thick;
            w -= thick;
        } else {
            // Fila horizontal pegada al borde superior: sus elementos van de izquierda a derecha.
            let thick = if last_row { h } else { (row_sum / w).min(h) };
            let mut cx = x;
            for (k, &v) in values.iter().enumerate().take(j).skip(i) {
                let a = v * scale;
                let len = if k + 1 == j && (last_row || exact) {
                    (x + w) - cx
                } else {
                    a / thick
                };
                out.push(Rect::new(
                    cx as f32,
                    y as f32,
                    len.max(0.0) as f32,
                    thick as f32,
                ));
                cx += len;
            }
            y += thick;
            h -= thick;
        }
        i = j;
    }
    debug_assert_eq!(out.len() - start, values.len());
}

/// `v > 0` (falso también para NaN), escrito como función para no negar una comparación de `f64`.
fn pos(v: f64) -> bool {
    v > 0.0
}
