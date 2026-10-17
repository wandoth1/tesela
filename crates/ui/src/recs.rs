//! Recomendaciones (PLAN.md §2.6): qué se puede limpiar, con riesgo y destino, calculado sobre el árbol escaneado (a
//! través del overlay de limpieza) y las raíces de las categorías que da el `cleaner`. La pantalla de Recomendaciones y
//! el titular "Hasta X en candidatos de bajo riesgo" salen de aquí.
//!
//! Los tamaños son una ESTIMACIÓN del árbol ("hasta"): la cifra definitiva la mide el `cleaner` al confirmar (con la
//! antigüedad real de cada archivo, las protecciones y los enlaces duros).

use crate::describe::{self, InfoKind, Special};
use crate::treepath::Locator;
use std::path::PathBuf;
use tesela_cleaner::{Backend, Browser, CategoryId, Risk, rules};
use tesela_model::{NodeId, Tree, View};

/// Una raíz concreta de una categoría (por ejemplo, la caché de un perfil de Chrome) con su tamaño.
#[derive(Clone, Debug)]
pub(crate) struct RecPart {
    pub node: NodeId,
    pub bytes: u64,
}

/// Una fila de la pantalla.
#[derive(Clone, Debug)]
pub(crate) struct RecRow {
    pub id: CategoryId,
    /// Lo que se estima liberar (con la antigüedad mínima de la regla aplicada con las fechas del árbol).
    pub bytes: u64,
    pub parts: Vec<RecPart>,
    /// El navegador está abierto: la fila no se preselecciona ("Cierra X para liberar hasta N").
    pub blocked_by: Option<Browser>,
}

impl RecRow {
    /// ¿Se puede marcar y preseleccionar ahora?
    pub fn usable(&self) -> bool {
        self.blocked_by.is_none() && self.bytes > 0
    }
}

/// Un archivo de solo informar (`pagefile.sys`, `hiberfil.sys`, `swapfile.sys`).
#[derive(Clone, Debug)]
pub(crate) struct InfoRow {
    pub kind: InfoKind,
    pub bytes: u64,
}

/// Por qué una categoría no está disponible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unavailable {
    /// Hay un enlace (junction, symlink, punto de montaje) en la ruta de su carpeta.
    Link,
    /// La carpeta cambió mientras se comprobaba.
    Changed,
    /// La carpeta temporal está redirigida fuera de los datos locales del usuario.
    Outside,
}

/// Una categoría que existe en el equipo pero que `cleaner` no ofrece, con el motivo (H4).
#[derive(Clone, Debug)]
pub(crate) struct UnavailableRow {
    pub id: CategoryId,
    pub why: Unavailable,
}

/// Todas las recomendaciones.
#[derive(Clone, Debug, Default)]
pub(crate) struct Recs {
    pub rows: Vec<RecRow>,
    /// Categorías que el `cleaner` descartó con un motivo que el usuario debe conocer (se enseñan como "no disponible").
    pub unavailable: Vec<UnavailableRow>,
    pub info: Vec<InfoRow>,
    pub downloads: Option<(NodeId, u64)>,
    pub recycle: Option<(NodeId, u64)>,
}

impl Recs {
    pub fn row(&self, id: CategoryId) -> Option<&RecRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// Lo seguro que se puede limpiar ahora (el titular y el botón "Limpiar lo seguro").
    pub fn safe_total(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| rules().get(r.id).risk == Risk::Safe && r.usable())
            .map(|r| r.bytes)
            .sum()
    }

    /// Lo que conviene mirar a mano (Revisar): Descargas y la Papelera.
    pub fn review_total(&self) -> u64 {
        self.downloads.map_or(0, |d| d.1) + self.recycle.map_or(0, |d| d.1)
    }

    /// Las categorías seguras que se pueden preseleccionar.
    pub fn preselected(&self) -> Vec<CategoryId> {
        self.rows
            .iter()
            .filter(|r| rules().get(r.id).preselected() && r.usable())
            .map(|r| r.id)
            .collect()
    }
}

/// Suma lo que cumple la antigüedad en el subárbol de `id`: `(bytes en disco, tamaño lógico, archivos, todo cumple)`. Un
/// archivo cumple si su `mtime` es anterior a `cutoff` (segundos Unix; 0 desconocido cuenta como antiguo). Con
/// `cutoff == u32::MAX` cumple todo.
pub(crate) fn eligible_by_age(view: &View, id: NodeId, cutoff: u32) -> (u64, u64, u64, bool) {
    let tree = view.tree();
    let n = tree.node(id);
    if !n.is_dir() {
        let ok = cutoff == u32::MAX || tree.mtime(id) < cutoff;
        return if ok {
            (view.alloc(id), view.logical(id), 1, true)
        } else {
            (0, 0, 0, false)
        };
    }
    let (mut b, mut l, mut f, mut all) = (0u64, 0u64, 0u64, true);
    let mut stack = vec![id];
    while let Some(d) = stack.pop() {
        for c in view.kids(d).iter() {
            let cn = tree.node(c);
            if cn.flags & tesela_model::flags::SYSTEM_INFO != 0 {
                continue;
            }
            if cn.is_dir() {
                stack.push(c);
            } else if cutoff == u32::MAX || tree.mtime(c) < cutoff {
                b += view.alloc(c);
                l += view.logical(c);
                f += 1;
            } else {
                all = false;
            }
        }
    }
    (b, l, f, all)
}

/// Calcula las recomendaciones. `now_unix`: "ahora" (el reloj, o el fijo de la demostración).
pub(crate) fn compute(
    tree: &Tree,
    view: &View,
    backend: &dyn Backend,
    browsers_open: &[Browser],
    now_unix: u32,
) -> Recs {
    let mut loc = Locator::new(tree);
    let mut recs = Recs::default();
    for id in [
        CategoryId::UserTemp,
        CategoryId::WindowsTemp,
        CategoryId::ChromeCache,
        CategoryId::EdgeCache,
        CategoryId::FirefoxCache,
    ] {
        let rule = rules().get(id);
        let cutoff = if rule.min_age.is_zero() {
            u32::MAX
        } else {
            now_unix.saturating_sub(rule.min_age.as_secs() as u32)
        };
        let mut parts = Vec::new();
        for path in backend.category_roots(id) {
            let Some(node) = loc.locate(&path) else {
                continue;
            };
            if !view.is_visible(node) {
                continue;
            }
            let (bytes, ..) = eligible_by_age(view, node, cutoff);
            parts.push(RecPart { node, bytes });
        }
        if parts.is_empty() {
            // Descartada por seguridad (un enlace en su ruta...): no desaparece en silencio. Solo se dice si la raíz descartada
            // está DENTRO de lo que se analizó (no basta con que sea de la misma unidad: al analizar una carpeta, una raíz
            // de otra parte de la unidad no se ve en este árbol).
            let on_this_drive = |p: &std::path::Path| loc.covers(p);
            if let Some((_, why)) = backend
                .unavailable_roots(id)
                .into_iter()
                .find(|(p, _)| on_this_drive(p))
            {
                use tesela_cleaner::roots::RootRejection as R;
                let why = match why {
                    R::ReparseInChain { .. } => Unavailable::Link,
                    R::NotCanonical => Unavailable::Changed,
                    R::OutsideLocalAppData => Unavailable::Outside,
                    R::Os(_) => continue,
                };
                recs.unavailable.push(UnavailableRow { id, why });
            }
            continue;
        }
        let bytes = parts.iter().map(|p| p.bytes).sum();
        let blocked_by = Browser::from_category(id).filter(|b| browsers_open.contains(b));
        recs.rows.push(RecRow {
            id,
            bytes,
            parts,
            blocked_by,
        });
    }
    // Descargas y Papelera: solo se enseñan (Revisar).
    if let Some(d) = backend.downloads_dir()
        && let Some(n) = loc.locate(&d)
        && view.is_visible(n)
    {
        recs.downloads = Some((n, view.alloc(n)));
    }
    let drive_root = PathBuf::from(tree.name_lossy(tree.root()).into_owned());
    if describe::is_drive_root_name(drive_root.to_string_lossy().trim_end_matches('\\'))
        && let Some(r) = backend.recycle_dir(&drive_root)
        && let Some(n) = loc.locate(&r)
        && view.is_visible(n)
    {
        recs.recycle = Some((n, view.alloc(n)));
    }
    // Archivos de solo informar en la raíz del volumen.
    for c in view.kids(tree.root()).iter() {
        if let Some(Special::InfoOnly(kind)) = describe::special(tree, c) {
            recs.info.push(InfoRow {
                kind,
                bytes: view.alloc(c),
            });
        }
    }
    recs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::{self, DEMO_NOW, Variant};

    /// Árbol de una sola carpeta (`name` es la ruta que se "escaneó").
    fn tree_of(name: &str) -> Tree {
        use tesela_model::{Batch, EntryData, FinishOptions, TreeBuilder, flags};
        let mut b = TreeBuilder::new();
        let mut root = Batch::detached(0);
        root.push(
            0,
            name.as_bytes(),
            EntryData {
                flags: flags::DIR,
                ..EntryData::default()
            },
        );
        b.add_batch(root).unwrap();
        b.add_batch(Batch::listing(0, 1)).unwrap();
        b.finish(&FinishOptions::new(DEMO_NOW))
    }

    /// GM-m7: una raíz descartada solo se enseña si está DENTRO de lo analizado, no por ser de la misma unidad.
    #[test]
    fn una_raiz_descartada_solo_se_enseña_si_esta_dentro_de_lo_analizado() {
        use tesela_cleaner::roots::RootRejection;
        let be = crate::demo_backend::DemoBackend::new(std::time::Duration::ZERO);
        be.set_unavailable(
            CategoryId::EdgeCache,
            RootRejection::ReparseInChain {
                component: "User Data".into(),
            },
        );
        let unavailable = |t: &Tree| {
            let view = View::new(t, None);
            compute(t, &view, &be, &[], DEMO_NOW).unavailable.len()
        };
        // El respaldo de la demostración descarta una raíz suya dentro de la unidad: con la unidad entera, sí.
        assert_eq!(unavailable(&tree_of("C:\\")), 1);
        assert_eq!(unavailable(&tree_of("C:\\Users")), 1);
        // En otra carpeta de la MISMA unidad (letra C), no: no es de lo que se está viendo.
        assert_eq!(unavailable(&tree_of("C:\\Otra")), 0);
        assert_eq!(unavailable(&tree_of("C:\\Otra\\Sub")), 0);
    }

    #[test]
    fn la_antiguedad_solo_cuenta_lo_que_cumple() {
        let t = demo::tree(Variant::Complete);
        let view = View::new(&t, None);
        let mut loc = Locator::new(&t);
        let root = t.name_lossy(0).into_owned();
        let temp = loc
            .locate(std::path::Path::new(&format!(
                "{root}Users\\{}\\AppData\\Local\\Temp",
                demo::USER
            )))
            .unwrap();
        let all = eligible_by_age(&view, temp, u32::MAX);
        let old = eligible_by_age(&view, temp, DEMO_NOW - 86_400);
        assert!(all.3 && !old.3, "con antigüedad no todo cumple");
        assert_eq!(all.0, t.node(temp).alloc);
        assert!(old.0 > 0 && old.0 < all.0, "{old:?} {all:?}");
        // Los archivos de 0 a 12 días: algo más del 90 % tiene más de un día.
        assert!(old.0 * 100 / all.0 > 70);
    }
}
