//! Ventana principal (PLAN.md §2.3): barra superior con breadcrumb, buscador y "Colorear", barra lateral con
//! unidades, vistas y leyenda, el mapa o la lista, el panel de detalles, la zona de la cesta (visible pero vacía e
//! inactiva: es del incremento 3) y la barra de estado.
//!
//! El mapa, la lista y los detalles leen el MISMO estado (`root`, `selection`): cambiar la selección en uno la cambia
//! en los demás.

use crate::app::{Action, Env, ScanRequest};
use crate::args;
use crate::clean::{CleanFlow, FlowEvent, SummaryClick};
use crate::demo;
use crate::describe::{self, Special};
use crate::home::{drive_title, settings_button};
use crate::icons::Icon;
use crate::map::MapState;
use crate::recs::Recs;
use crate::theme::{self, Palette};
use crate::widgets;
use crate::{details, list};
use egui::{
    Align, CornerRadius, Key, Layout, Margin, Modifiers, RichText, Sense, Stroke, Ui, vec2,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tesela_cleaner::{Backend, Browser, Protection};
use tesela_layout::{Others, Root};
use tesela_model::{FileType, NodeId, Overlay, Tree, View as TreeView};
use tesela_scanner::ScanResult;

/// Cifras del escaneo que se conservan junto al árbol.
#[derive(Clone, Debug)]
pub struct ScanInfo {
    pub req: ScanRequest,
    /// Ruta escaneada tal como se muestra.
    pub display_root: String,
    /// Se escaneó la unidad entera (hay bloque del sistema y "espacio explicado").
    pub full_volume: bool,
    /// (total, libre) del volumen al terminar.
    pub volume: Option<(u64, u64)>,
    /// `total` y `libre` son del VOLUMEN. `false` con una cuota cuando Windows no da la capacidad real: son las cifras de
    /// la cuenta del usuario y la barra de estado lo avisa (igual que la tarjeta de inicio).
    pub volume_scope: bool,
    /// Se paró con "Parar y ver": las cifras con "≥" son un mínimo.
    pub cancelled: bool,
    /// Escaneo en modo de respaldo (`FindFirstFileExW`): sin deduplicación de enlaces duros.
    pub approximate: bool,
    /// Carpetas que no se pudieron leer (permisos, errores).
    pub dir_errors: u64,
    pub files: u64,
    pub elapsed: Duration,
    pub finished_at: Instant,
}

/// Qué está seleccionado.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sel {
    Node(NodeId),
    /// La agrupación "Otros".
    Group(Others),
}

/// Vista del área central.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Map,
    List,
    Recommendations,
}

/// Qué se puede hacer con un elemento respecto a la cesta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddState {
    /// Se puede añadir.
    Addable,
    /// Ya está en la cesta (él o un ancestro).
    InBasket,
    /// Carpeta conocida: solo "Contenido de …".
    ContentsOf,
    /// Intocable como destino (raíz del perfil, AppData...): su contenido sí se puede limpiar.
    Untouchable,
    /// Denegada por completo.
    Denied,
    /// `pagefile.sys` y compañía: solo informar.
    InfoOnly,
    /// El bloque "Windows y otros datos del sistema".
    SystemBlock,
    /// Archivo que solo está en la nube: no se puede seleccionar para liberar espacio.
    CloudOnly,
    /// Una agrupación "Otros": se abre y se elige dentro.
    Group,
}

/// Exploración de un árbol.
pub struct Explore {
    pub(crate) tree: Arc<Tree>,
    /// Lo que ya se limpió, sin reescanear (PLAN.md §3.6). `None` hasta la primera limpieza.
    pub(crate) overlay: Option<Arc<Overlay>>,
    /// Ya se limpió algo desde el análisis: los histogramas (tipos, antigüedad) son aproximados hasta reescanear.
    pub(crate) cleaned: bool,
    pub(crate) info: ScanInfo,
    /// Versión de los datos para la caché del layout (cambia con el árbol; en el inc. 3, con el overlay).
    pub(crate) version: u64,
    /// Escrituras que traen los nombres del árbol (para cargar las fuentes de reserva que hagan falta).
    pub(crate) scripts: crate::fonts::Scripts,
    pub(crate) root: Root,
    /// Raíces anteriores al abrir agrupaciones "Otros" anidadas (Retroceso vuelve por ellas).
    pub(crate) group_stack: Vec<Root>,
    /// El contenido de la barra lateral no cabe y se desplaza (se muestra su barra).
    pub(crate) sidebar_overflow: bool,
    /// El panel de detalles no cabe y se desplaza (con su barra visible).
    pub(crate) details_overflow: bool,
    pub(crate) selection: Option<Sel>,
    /// Selección múltiple: lo marcado para añadirlo junto a la cesta (hijos de la carpeta actual; ver `multisel`).
    pub(crate) multi: crate::multisel::Marks,
    /// La búsqueda (plegada) con la que se ajustaron las marcas por última vez.
    pub(crate) marks_search: String,
    /// Marcas que soltó una navegación y todavía no se avisaron (el aviso sale en el siguiente fotograma).
    pub(crate) marks_dropped: usize,
    /// Estimación de la cesta (entradas, bytes) con la versión de la cesta y del árbol con que se calculó.
    pub(crate) estimate: Option<((u64, u64), (usize, u64))>,
    /// Índice de hermanos del `Locator` de la ventana "Ver" mientras está abierta (con la versión del árbol y la de la cesta
    /// con que se construyó): cada fotograma solo localiza las filas que se ven, sin reconstruir los índices.
    pub(crate) locator_cache: Option<((u64, u64), crate::treepath::Index)>,
    /// Cuántas veces se recalculó la estimación (las pruebas comprueban que no es una vez por fotograma).
    pub(crate) estimate_computes: u32,
    pub(crate) view: View,
    /// El flujo de limpieza: cesta, confirmación, ejecución y resumen.
    pub(crate) flow: CleanFlow,
    /// Recomendaciones calculadas (se recalculan al cambiar el árbol o los navegadores abiertos).
    pub(crate) recs: Option<(u64, Recs)>,
    /// Ya se preseleccionó lo seguro al abrir Recomendaciones por primera vez.
    pub(crate) recs_preselected: bool,
    pub(crate) basket_open: bool,
    pub(crate) browsers_open: Vec<Browser>,
    pub(crate) protection: Arc<Protection>,
    pub(crate) now_unix: u32,
    pub(crate) search: String,
    pub(crate) map: MapState,
    pub(crate) scroll_to_selection: bool,
    /// Fila de la lista a la que debe pasar el foco de teclado en el siguiente pase.
    pub(crate) list_focus: Option<NodeId>,
    pub(crate) animate: bool,
    /// Elemento sobre el que se abrió el menú contextual.
    pub(crate) ctx_target: Option<Sel>,
    pub(crate) toast: Option<(String, f64)>,
    /// Petición de abrir el Explorador en curso (nunca bloquea: se consulta en cada frame).
    reveal: Option<crate::platform::RevealHandle>,
    focus_search: bool,
    /// El buscador tiene el foco del teclado (las teclas de navegación no actúan mientras se escribe).
    search_focused: bool,
    /// Último widget de navegación (el mapa o una fila de la lista) que tuvo el foco. Intro, Retroceso y las flechas
    /// solo actúan si el foco está en él o en ningún widget: con otro control enfocado (un botón, un enlace...) las
    /// teclas son suyas.
    pub(crate) nav_focus: Option<egui::Id>,
    search_hits: Option<(String, Root, Arc<Vec<NodeId>>)>,
    /// Los mismos aciertos como conjunto (para la veladura del mapa: pertenencia en O(1)).
    search_set: Option<(String, Root, Arc<std::collections::HashSet<NodeId>>)>,
}

/// Los hijos que se están mostrando (la carpeta entera o una agrupación "Otros" abierta).
pub(crate) struct Shown {
    kids: tesela_layout::Children,
    start: usize,
    end: usize,
}

impl Shown {
    pub(crate) fn len(&self) -> usize {
        self.end - self.start
    }
    pub(crate) fn get(&self, i: usize) -> NodeId {
        self.kids.get(self.start + i)
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = NodeId> + '_ {
        (self.start..self.end).map(|i| self.kids.get(i))
    }
}

/// Número que identifica el árbol en la caché (un contador por proceso).
fn next_version() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static V: AtomicU64 = AtomicU64::new(1);
    V.fetch_add(1, Ordering::Relaxed)
}

impl Explore {
    fn new(tree: Tree, info: ScanInfo, animate: bool, backend: &Arc<dyn Backend>) -> Explore {
        let scripts = crate::fonts::scripts_of_wtf8(tree.names_bytes());
        Explore {
            tree: Arc::new(tree),
            overlay: None,
            cleaned: false,
            info,
            version: next_version(),
            scripts,
            root: Root::node(0),
            group_stack: Vec::new(),
            sidebar_overflow: false,
            details_overflow: false,
            selection: None,
            multi: crate::multisel::Marks::default(),
            marks_search: String::new(),
            marks_dropped: 0,
            estimate: None,
            locator_cache: None,
            estimate_computes: 0,
            view: View::Map,
            flow: CleanFlow::default(),
            recs: None,
            recs_preselected: false,
            basket_open: false,
            browsers_open: backend.running_browsers(),
            protection: backend.protection(),
            now_unix: if backend.is_demo() {
                crate::demo::DEMO_NOW
            } else {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as u32)
            },
            search: String::new(),
            map: MapState::default(),
            scroll_to_selection: false,
            list_focus: None,
            animate,
            ctx_target: None,
            toast: None,
            reveal: None,
            focus_search: false,
            search_focused: false,
            nav_focus: None,
            search_hits: None,
            search_set: None,
        }
    }

    /// Desde el resultado de un escaneo real.
    pub fn from_scan(
        req: ScanRequest,
        res: ScanResult,
        animate: bool,
        elapsed: Duration,
        backend: &Arc<dyn Backend>,
    ) -> Explore {
        let files = res.tree.stats().files;
        let info = crate::scan::info_from_summary(&req, &res.summary, files, elapsed);
        Explore::new(res.tree, info, animate, backend)
    }

    /// Desde el árbol sintético de la demostración.
    pub fn demo(
        req: ScanRequest,
        variant: demo::Variant,
        animate: bool,
        backend: Arc<dyn Backend>,
    ) -> Explore {
        let tree = demo::tree(variant);
        Explore::demo_from_tree(
            req,
            tree,
            variant == demo::Variant::Stopped,
            animate,
            backend,
        )
    }

    /// Como [`Explore::demo`] con un árbol dado (las pruebas de escala usan uno con miles de hijos).
    pub(crate) fn demo_from_tree(
        req: ScanRequest,
        tree: Tree,
        cancelled: bool,
        animate: bool,
        backend: Arc<dyn Backend>,
    ) -> Explore {
        let files = tree.stats().files;
        let volume = req.drive.as_ref().map(|d| (d.total, d.free));
        let volume_scope = req.drive.as_ref().is_none_or(|d| d.volume_scope);
        let info = ScanInfo {
            display_root: req.root.display().to_string(),
            full_volume: true,
            volume,
            volume_scope,
            cancelled,
            approximate: false,
            dir_errors: 0,
            files,
            elapsed: Duration::from_secs(6),
            finished_at: Instant::now(),
            req,
        };
        Explore::new(tree, info, animate, &backend)
    }

    /// Coloca la exploración de la demostración según `start` (carpetas por nombre; sin animaciones).
    pub fn demo_navigate(&mut self, start: &crate::app::MapStart) {
        for name in &start.open {
            if let Some(c) = self.tree.child_by_name(self.root.node, name.as_bytes()) {
                self.root = Root::node(c);
            }
        }
        if let Some(n) = &start.select
            && let Some(id) = self
                .shown_children()
                .iter()
                .find(|&c| self.tree.name_lossy(c) == n.as_str())
        {
            self.selection = Some(Sel::Node(id));
        }
        if !start.marks.is_empty() {
            let marked = self
                .shown_children()
                .iter()
                .filter(|&id| {
                    start
                        .marks
                        .iter()
                        .any(|name| self.tree.name_lossy(id) == name.as_str())
                })
                .collect::<Vec<_>>();
            self.multi.add(marked);
        }
        if start.list {
            self.view = View::List;
        }
    }

    /// Coloca la exploración de la demostración en una pantalla del flujo de limpieza (capturas y pruebas; el respaldo es
    /// el simulado: no toca el disco).
    pub fn demo_clean(
        &mut self,
        backend: &Arc<dyn Backend>,
        what: crate::app::CleanStart,
        dry: bool,
    ) {
        use crate::app::CleanStart;
        self.recs_preselected = false;
        let env_free = self.volume_ctx();
        match what {
            CleanStart::Recommendations => {
                self.view = View::Recommendations;
                self.sync_blocked_hint();
            }
            CleanStart::Confirm | CleanStart::Summary => {
                // La cesta de ejemplo: lo seguro que se puede limpiar ahora, una carpeta de proyectos y un archivo tuyo.
                let recs = {
                    let view = self.view();
                    crate::recs::compute(
                        &self.tree,
                        &view,
                        backend.as_ref(),
                        &self.browsers_open,
                        self.now_unix,
                    )
                };
                for id in recs.preselected() {
                    self.flow.basket.set_category(id, true);
                }
                self.recs = Some((self.version, recs));
                self.recs_preselected = true;
                self.sync_blocked_hint();
                let prot = self.protection.clone();
                let base = format!("{}Users\\{}", crate::demo::root_name(), crate::demo::USER);
                for rel in [
                    "Proyectos\\tienda-web\\node_modules",
                    "Descargas\\distro-linux-24.04.iso",
                ] {
                    self.flow
                        .basket
                        .add_path(std::path::PathBuf::from(format!("{base}\\{rel}")), &prot);
                }
                if what == CleanStart::Confirm {
                    self.flow.open_confirm_now(backend, env_free);
                } else {
                    self.flow.run_now(backend, env_free, dry);
                    for ev in self.flow.take_events() {
                        let FlowEvent::Removed(v) = ev;
                        self.apply_removed(&v);
                    }
                }
            }
        }
    }

    /// Pasa a la limpieza las cachés de navegadores abiertos que no se limpian y cuánto se liberaría al cerrarlos.
    pub(crate) fn sync_blocked_hint(&mut self) {
        self.flow.blocked_hint = self
            .recs
            .as_ref()
            .map(|(_, r)| {
                r.rows
                    .iter()
                    .filter_map(|row| row.blocked_by.map(|b| (b, row.bytes)))
                    .collect()
            })
            .unwrap_or_default();
    }

    /// Nombre de lo explorado (para el título de la ventana).
    pub fn title(&self) -> String {
        self.info.req.label.clone()
    }

    // --- Navegación ---------------------------------------------------------------------------------------------

    /// ¿Se puede volver a la pantalla de inicio? No mientras se ejecuta una limpieza (como el cierre de la ventana).
    pub(crate) fn can_go_home(&self) -> bool {
        !self.flow.is_running()
    }

    /// ¿Se puede subir un nivel?
    pub fn can_up(&self) -> bool {
        self.root.is_group() || self.root.node != 0
    }

    /// Sube un nivel (de una agrupación "Otros" vuelve a su carpeta). La carpeta de la que se sale queda seleccionada.
    pub fn up(&mut self) {
        if !self.can_up() {
            return;
        }
        let from = self.root;
        let (target, select) = if from.is_group() {
            (
                self.group_stack.pop().unwrap_or(Root::node(from.node)),
                None,
            )
        } else {
            let parent = self.tree.parent(from.node).unwrap_or(0);
            (Root::node(parent), Some(Sel::Node(from.node)))
        };
        self.map.begin_zoom_out(self.animate, from);
        self.root = target;
        self.selection = select;
        // Lo marcado habla de la carpeta que se deja: no se arrastra a la nueva.
        self.drop_marks_for_navigation();
        self.scroll_to_selection = true;
    }

    /// Entra en un elemento: una carpeta (que pasa a ser la raíz del mapa) o una agrupación "Otros" (que se abre).
    /// Con un archivo no hace nada. Devuelve si cambió la raíz.
    pub fn enter(&mut self, sel: Sel) -> bool {
        let target = match sel {
            Sel::Node(id) if describe::can_enter_view(&self.view(), id) => Root::node(id),
            Sel::Group(o) => o.root(),
            _ => return false,
        };
        if target == self.root {
            return false;
        }
        self.map.begin_zoom_in(self.animate, sel);
        if matches!(sel, Sel::Group(_)) {
            self.group_stack.push(self.root);
        } else {
            self.group_stack.clear();
        }
        self.root = target;
        self.selection = None;
        self.drop_marks_for_navigation();
        self.scroll_to_selection = true;
        true
    }

    /// Salta a una carpeta cualquiera (breadcrumb).
    pub fn go_to(&mut self, id: NodeId) {
        if Root::node(id) == self.root {
            return;
        }
        let ancestor = {
            let mut cur = self.root.node;
            let mut found = false;
            while let Some(p) = self.tree.parent(cur) {
                if p == id {
                    found = true;
                    break;
                }
                cur = p;
            }
            found || self.root.is_group() && self.root.node == id
        };
        if ancestor {
            self.map.begin_zoom_out(self.animate, self.root);
        }
        self.root = Root::node(id);
        self.group_stack.clear();
        self.selection = None;
        self.drop_marks_for_navigation();
        self.scroll_to_selection = true;
    }

    /// Navegar suelta lo marcado (habla de la carpeta que se deja) y lo anota para avisarlo.
    fn drop_marks_for_navigation(&mut self) {
        self.marks_dropped += self.multi.len();
        self.multi.clear();
    }

    /// Selecciona (o limpia la selección).
    pub fn select(&mut self, sel: Option<Sel>) {
        self.selection = sel;
    }

    /// El árbol visto a través del overlay de limpieza.
    pub(crate) fn view(&self) -> TreeView<'_> {
        TreeView::new(&self.tree, self.overlay.as_deref())
    }

    /// Hijos (visibles) que está mostrando ahora el mapa o la lista.
    pub(crate) fn shown_children(&self) -> Shown {
        let kids = tesela_layout::Children::of_view(&self.view(), self.root.node);
        let end = (self.root.end as usize).min(kids.len());
        let start = (self.root.start as usize).min(end);
        Shown { kids, start, end }
    }

    /// Aplica al árbol lo que desapareció de verdad (overlay, PLAN.md §3.6), sin reescanear: lo que se eliminó entero
    /// deja de verse y lo que se limpió solo en parte encoge. Los ancestros restan lo mismo.
    pub(crate) fn apply_removed(&mut self, removed: &[tesela_cleaner::Removed]) {
        if removed.is_empty() {
            return;
        }
        let tree = self.tree.clone();
        let mut loc = crate::treepath::Locator::new(&tree);
        let ov = Arc::make_mut(
            self.overlay
                .get_or_insert_with(|| Arc::new(Overlay::new(&tree))),
        );
        for r in removed {
            let Some(id) = loc.locate(&r.path) else {
                continue;
            };
            if r.removed {
                ov.remove(&tree, id);
            } else {
                ov.shrink(&tree, id, r.bytes, r.logical, r.files);
            }
        }
        self.cleaned = true;
        // La caché del layout y el mesh se invalidan por la versión; la selección y la carpeta actual se mantienen
        // válidas.
        self.version = next_version();
        self.search_hits = None;
        self.search_set = None;
        let view = TreeView::new(&self.tree, self.overlay.as_deref());
        // Lo que ya no existe deja de estar marcado (lo demás se conserva).
        self.multi.retain(|id| view.is_visible(id));
        if let Some(Sel::Node(id)) = self.selection
            && !view.is_visible(id)
        {
            self.selection = None;
        }
        if let Some(Sel::Group(o)) = self.selection
            && !view.is_visible(o.parent)
        {
            self.selection = None;
        }
        let mut node = self.root.node;
        let mut moved = false;
        while !view.is_visible(node) {
            node = self.tree.parent(node).unwrap_or(0);
            moved = true;
        }
        if moved || self.root.is_group() {
            // Una agrupación "Otros" abierta ya no describe lo mismo: se vuelve a la carpeta.
            self.root = Root::node(node);
            self.group_stack.clear();
            self.multi.clear();
        }
    }

    /// Texto de la ruta de un elemento (solo para mostrar o copiar).
    pub(crate) fn path_of(&self, sel: Sel) -> String {
        match sel {
            Sel::Node(id) => self.tree.path_lossy(id),
            Sel::Group(o) => self.tree.path_lossy(o.parent),
        }
    }

    /// Nodos de la carpeta actual cuyo nombre contiene la búsqueda (sin distinguir mayúsculas), o `None` sin búsqueda.
    /// Los aciertos de la búsqueda como conjunto, o `None` sin búsqueda. Se recalcula solo si cambia la búsqueda o la raíz.
    pub(crate) fn search_set(&mut self) -> Option<Arc<std::collections::HashSet<NodeId>>> {
        let hits = self.search_matches()?;
        let q = describe::fold(self.search.trim());
        if let Some((cq, croot, set)) = &self.search_set
            && *cq == q
            && *croot == self.root
        {
            return Some(set.clone());
        }
        let set: Arc<std::collections::HashSet<NodeId>> = Arc::new(hits.iter().copied().collect());
        self.search_set = Some((q, self.root, set.clone()));
        Some(set)
    }

    pub(crate) fn search_matches(&mut self) -> Option<Arc<Vec<NodeId>>> {
        let q = describe::fold(self.search.trim());
        if q.is_empty() {
            self.search_hits = None;
            return None;
        }
        if let Some((cq, croot, hits)) = &self.search_hits
            && *cq == q
            && *croot == self.root
        {
            return Some(hits.clone());
        }
        let hits: Vec<NodeId> = self
            .shown_children()
            .iter()
            .filter(|&c| describe::fold(&self.tree.name_lossy(c)).contains(&q))
            .collect();
        let hits = Arc::new(hits);
        self.search_hits = Some((q, self.root, hits.clone()));
        Some(hits)
    }

    // --- Pintado --------------------------------------------------------------------------------------------------

    /// Pinta la ventana principal.
    pub fn show(&mut self, ui: &mut Ui, env: &mut Env) {
        let ctx = ui.ctx().clone();
        // El flujo de limpieza avanza (hilos de aprobación y ejecución) y lo que desapareció se aplica al árbol.
        self.flow.poll(&ctx, env.backend, env.dry_run);
        for ev in self.flow.take_events() {
            let FlowEvent::Removed(v) = ev;
            self.apply_removed(&v);
        }
        self.ensure_recs(env);
        self.sync_blocked_hint();
        if self.flow.recheck {
            self.flow.recheck = false;
            self.recheck_browsers(env);
            self.view = View::Recommendations;
        }
        // El resumen ocupa toda la ventana.
        if self.flow.in_summary() {
            match self.flow.show_summary(ui, env) {
                Some(SummaryClick::EmptyNow) => {
                    self.flow.close_summary();
                    self.flow.basket.clear();
                    self.flow.basket.set_empty_bin(true);
                    self.start_clean(&ctx, env);
                }
                Some(SummaryClick::OpenBin) => crate::platform::open_recycle_bin(),
                _ => {}
            }
            return;
        }
        self.handle_keys(&ctx, env);
        let pal = env.pal;

        // Orden: lo que se añade primero queda más afuera.
        egui::Panel::bottom("tesela-status")
            .exact_size(32.0)
            .frame(bar_frame(pal.surface_alt))
            .show(ui, |ui| self.status_bar(ui, env));
        egui::Panel::bottom("tesela-basket")
            .exact_size(56.0)
            .frame(bar_frame(pal.surface))
            .show(ui, |ui| self.basket_bar(ui, env));
        egui::Panel::top("tesela-toolbar")
            .exact_size(56.0)
            .frame(bar_frame(pal.surface))
            .show(ui, |ui| self.toolbar(ui, env));
        // La búsqueda pudo cambiar en la barra superior: lo marcado que ya no es un acierto deja de estarlo.
        self.sync_marks_with_search();
        egui::Panel::left("tesela-sidebar")
            .exact_size(232.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .stroke(Stroke::NONE)
                    .inner_margin(Margin::symmetric(12, 16)),
            )
            .show(ui, |ui| {
                // Si el contenido no cabe (ventana pequeña) la barra de desplazamiento es SÓLIDA y siempre visible: nada queda
                // cortado sin aviso.
                ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                let out = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.sidebar(ui, env));
                self.sidebar_overflow = out.content_size.y > out.inner_rect.height() + 0.5;
            });
        // Las Recomendaciones ocupan todo el ancho (como en la maqueta): sin panel de detalles.
        if self.view != View::Recommendations {
            egui::Panel::right("tesela-details")
                .exact_size(300.0)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(pal.surface)
                        .inner_margin(Margin::same(20)),
                )
                .show(ui, |ui| details::show(ui, env, self));
        }
        // La barra de selección va siempre (mismo alto) y solo en la columna central, tras los paneles laterales: si
        // apareciese solo al marcar, el mapa se recolocaría bajo el puntero, y quitar alto a la barra lateral la haría desbordar.
        if self.view != View::Recommendations {
            let fill = if self.multi.is_empty() {
                pal.bg
            } else {
                pal.accent_soft
            };
            egui::Panel::bottom("tesela-selection")
                .exact_size(crate::multisel::BAR_H)
                .frame(bar_frame(fill))
                .show(ui, |ui| self.selection_bar(ui, env));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(Margin::same(16)),
            )
            .show(ui, |ui| match self.view {
                View::Map => crate::map::show(ui, env, self),
                View::List => list::show(ui, env, self),
                View::Recommendations => self.recs_ui(ui, env),
            });
        self.basket_window(&ctx, env);
        let vol = self.volume_ctx();
        let used = vol.volume.map(|(t, f)| t.saturating_sub(f));
        self.flow.show_modals(&ctx, env, vol, used);
        // Desenlace de "Abrir en el Explorador": el hilo suelto responde por el manejador; un shell que no responde o un
        // error salen como aviso.
        if let Some(h) = &mut self.reveal {
            if let Some(outcome) = h.poll() {
                self.reveal = None;
                if let Some(msg) = crate::error::reveal_outcome_message(env.tr, &outcome) {
                    self.toast = Some((msg, ctx.input(|i| i.time) + 4.0));
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
        // Navegar soltó marcas: un aviso breve (no se pisa otro aviso más reciente de la propia navegación).
        if self.marks_dropped > 0 {
            let n = std::mem::take(&mut self.marks_dropped) as u64;
            let msg = env.tr.ta("sel-dropped", args!["count" => env.tr.int(n)]);
            self.toast = Some((msg, ctx.input(|i| i.time) + 3.0));
        }
        // Aviso temporal ("Ruta copiada"): encima de la barra de la cesta y, si hay, de la barra de selección.
        let toast_lift = -110.0
            - if self.view == View::Recommendations {
                0.0
            } else {
                46.0
            };
        if let Some((msg, until)) = &self.toast {
            let now = ctx.input(|i| i.time);
            if now < *until {
                egui::Area::new(egui::Id::new("tesela-toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, toast_lift))
                    .order(egui::Order::Tooltip)
                    .show(&ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.label(msg);
                        });
                    });
                ctx.request_repaint_after(Duration::from_millis(200));
            } else {
                self.toast = None;
            }
        }
    }

    /// Abre el Explorador con `path` seleccionado SIN bloquear la interfaz. Si ya hay una petición sin terminar o se
    /// alcanzó el tope de hilos, avisa con un toast en vez de lanzar otra.
    pub(crate) fn reveal(&mut self, path: &std::path::Path, tr: &crate::i18n::Tr, now: f64) {
        use crate::platform::{RevealOutcome, RevealStart, reveal_in_explorer_async};
        let busy = || {
            crate::error::reveal_outcome_message(tr, &RevealOutcome::Busy).map(|m| (m, now + 4.0))
        };
        if self.reveal.is_some() {
            self.toast = busy();
            return;
        }
        match reveal_in_explorer_async(path) {
            RevealStart::Started(h) => self.reveal = Some(h),
            RevealStart::Busy => self.toast = busy(),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context, env: &mut Env) {
        // Con una ventana modal abierta ("Acerca de") NINGÚN atajo de la exploración actúa, tampoco Ctrl+F ni F5.
        if env.modal_open || self.flow.modal_open() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.focus_search = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5)) {
            env.actions.push(Action::Rescan);
        }
        // Alt+Inicio: a la pantalla de inicio (no choca con Inicio a secas, que va a la primera fila de la lista: aquí el
        // atajo exige Alt). Con una limpieza en curso no hace nada, igual que el botón.
        if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, Key::Home)) && self.can_go_home() {
            env.actions.push(Action::GoHome);
        }
        if self.search_focused {
            return;
        }
        // Un menú abierto se queda con el teclado (Escape lo cierra, Intro activa su opción).
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        // Intro, Retroceso y las flechas son de la navegación SOLO si el foco está en el mapa, en una fila de la lista
        // o en ningún widget. Con un botón enfocado, Intro y Espacio lo activan (no se consumen aquí).
        let nav = match ctx.memory(|m| m.focused()) {
            None => true,
            Some(id) => self.nav_focus == Some(id),
        };
        if nav && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) {
            self.up();
        }
        // Ctrl+A: marca todo lo que se ve (los aciertos si hay búsqueda). El buscador tiene el suyo (ya salió arriba).
        if self.view != View::Recommendations
            && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A))
        {
            self.marks_all_here();
        }
        if nav
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
            && let Some(sel) = self.selection
        {
            self.enter(sel);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            // Primero se vacía lo marcado, luego la búsqueda y por último el elemento activo.
            if self.view != View::Recommendations && !self.multi.is_empty() {
                self.multi.clear();
            } else if !self.search.is_empty() {
                self.search.clear();
            } else {
                self.selection = None;
            }
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            // Inicio: la pantalla "¿Qué disco quieres revisar?" (con "Analizar una carpeta…"). A la izquierda de "Subir".
            let home_label = env.tr.t("go-home");
            if widgets::icon_button(ui, env, Icon::Home, &home_label, self.can_go_home()).clicked()
            {
                env.actions.push(Action::GoHome);
            }
            let up_label = env.tr.t("up-level");
            if widgets::icon_button(ui, env, Icon::ArrowUp, &up_label, self.can_up()).clicked() {
                self.up();
            }
            self.breadcrumb(ui, env);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                settings_button(ui, env);
                color_group(ui, env);
                // Buscador.
                ui.add_space(8.0);
                let hint = env.tr.t("search-hint");
                let framed = egui::Frame::new()
                    .fill(pal.bg)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 0))
                    .show(ui, |ui| {
                        ui.set_height(34.0);
                        ui.set_width(220.0);
                        ui.horizontal_centered(|ui| {
                            env.icons.show(ui, Icon::Search, 16.0, pal.text_muted);
                            let te = egui::TextEdit::singleline(&mut self.search)
                                .id(egui::Id::new("tesela-search"))
                                .hint_text(hint.clone())
                                .frame(egui::Frame::NONE)
                                .desired_width(f32::INFINITY);
                            let r = ui.add(te);
                            r.widget_info(|| egui::WidgetInfo {
                                label: Some(env.tr.t("search")),
                                ..egui::WidgetInfo::text_edit(
                                    true,
                                    "",
                                    self.search.clone(),
                                    hint.clone(),
                                )
                            });
                            r
                        })
                        .inner
                    });
                let resp = framed.inner;
                if self.focus_search {
                    resp.request_focus();
                    self.focus_search = false;
                }
                self.search_focused = resp.has_focus();
                if self.search_focused {
                    widgets::focus_ring(ui.painter(), &pal, framed.response.rect, 8);
                }
            });
        });
    }

    fn breadcrumb(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        // Cadena de nodos desde la raíz hasta la carpeta actual.
        let mut chain = vec![self.root.node];
        while let Some(p) = self.tree.parent(*chain.last().unwrap()) {
            chain.push(p);
        }
        chain.reverse();
        let group = self.root.is_group().then(|| {
            let sh = self.shown_children();
            let view = self.view();
            let bytes: u64 = sh.iter().map(|c| view.alloc(c)).sum();
            (sh.len(), bytes)
        });
        let last_is_current = group.is_none();
        let skip = chain.len().saturating_sub(4);
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        if skip > 0 {
            let target = chain[skip - 1];
            let more = ui.link("…");
            if more.has_focus() {
                widgets::focus_ring(ui.painter(), &pal, more.rect.expand2(vec2(3.0, 1.0)), 4);
            }
            if more.clicked() {
                self.go_to(target);
            }
            env.icons.show(ui, Icon::ChevronRight, 12.0, pal.text_muted);
        }
        let shown = chain[skip..].to_vec();
        for (i, &id) in shown.iter().enumerate() {
            let name = describe::crumb_name(&self.tree, id, env.tr);
            let is_last = i + 1 == shown.len();
            if is_last && last_is_current {
                ui.label(
                    RichText::new(&name)
                        .font(theme::font_semibold(14.0))
                        .color(pal.text),
                );
                let a = self.view().alloc(id);
                let sz = if describe::is_lower_bound(&self.tree, id) {
                    env.tr.size_min(a)
                } else {
                    env.tr.size(a)
                };
                ui.label(RichText::new(sz).color(pal.text_muted));
            } else {
                let r = ui.add(
                    egui::Label::new(RichText::new(&name).color(pal.text_muted))
                        .sense(Sense::click()),
                );
                r.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        env.tr.ta("go-to-folder", args!["name" => name.clone()]),
                    )
                });
                if r.has_focus() {
                    widgets::focus_ring(ui.painter(), &pal, r.rect.expand2(vec2(3.0, 1.0)), 4);
                }
                if r.clicked() {
                    self.go_to(id);
                }
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                env.icons.show(ui, Icon::ChevronRight, 12.0, pal.text_muted);
            }
        }
        if let Some((n, bytes)) = group {
            ui.label(
                RichText::new(env.tr.ta(
                    "others-title",
                    args!["n" => env.tr.int(n as u64), "size" => env.tr.size(bytes)],
                ))
                .font(theme::font_semibold(14.0))
                .color(pal.text),
            );
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        ui.spacing_mut().item_spacing.y = 6.0;
        widgets::section_label(ui, &pal, &env.tr.t("sidebar-drives"));
        let cur = self.info.req.drive.as_ref().map(|d| d.letter);
        let drives = env.drives.to_vec();
        for d in &drives {
            let selected = Some(d.letter) == cur;
            let title = drive_title(env, d);
            let used = d.used_percent();
            // Hueco para el fondo de "al pasar el puntero": se rellena cuando se sabe si la fila está bajo el puntero (la
            // respuesta se calcula DESPUÉS de dibujar el contenido) y queda por debajo de él.
            let hover_bg = ui.painter().add(egui::Shape::Noop);
            let resp = egui::Frame::new()
                .fill(if selected {
                    pal.accent_soft
                } else {
                    egui::Color32::TRANSPARENT
                })
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let f = if selected {
                            theme::font_semibold(14.0)
                        } else {
                            egui::FontId::proportional(14.0)
                        };
                        ui.label(RichText::new(&title).font(f).color(pal.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(env.tr.pct(used as f64))
                                    .size(12.0)
                                    .color(pal.text_muted),
                            );
                        });
                    });
                    let color = if used >= 90 { pal.review } else { pal.accent };
                    widgets::progress_bar(ui, &pal, used as f32 / 100.0, color, 6.0, false, None);
                })
                .response;
            let resp = ui.interact(
                resp.rect,
                ui.id().with(("unidad", d.letter)),
                Sense::click(),
            );
            resp.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    // La unidad actual no es pulsable: el lector de pantalla no la anuncia como botón habilitado.
                    drive_row_is_interactive(selected),
                    selected,
                    env.tr.ta("analyze-drive", args!["name" => title.clone()]),
                )
            });
            // Solo las unidades que NO son la actual se pueden pulsar: se iluminan al pasar el puntero (como los elementos de
            // la barra lateral), cambian el cursor y explican qué hacen. La actual no tiene estado de "pasar el puntero".
            let resp = if drive_row_is_interactive(selected) {
                if resp.hovered() {
                    ui.painter().set(
                        hover_bg,
                        egui::epaint::RectShape::filled(
                            resp.rect,
                            CornerRadius::same(8),
                            pal.accent_soft,
                        ),
                    );
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                resp.on_hover_text(env.tr.ta("analyze-drive", args!["name" => title.clone()]))
            } else {
                resp
            };
            if resp.has_focus() {
                widgets::focus_ring(ui.painter(), &pal, resp.rect, 8);
            }
            // `clicked` también se activa con Intro y Espacio cuando la fila tiene el foco.
            if resp.clicked() && drive_row_is_interactive(selected) {
                env.actions.push(Action::StartScan(ScanRequest {
                    root: d.root(),
                    label: title.clone(),
                    drive: Some(d.clone()),
                }));
            }
        }
        ui.add_space(14.0);
        widgets::section_label(ui, &pal, &env.tr.t("sidebar-views"));
        for (view, icon, key) in [
            (View::Map, Icon::DataTreemap, "view-map"),
            (View::List, Icon::TextBulletList, "view-list"),
        ] {
            let selected = self.view == view;
            let label = env.tr.t(key);
            if sidebar_item(ui, env, icon, &label, selected, true, None) {
                self.view = view;
            }
        }
        // Recomendaciones, con lo seguro que se puede liberar como chip.
        self.ensure_recs(env);
        let safe = self.recs.as_ref().map_or(0, |r| r.1.safe_total());
        let chip = (safe > 0).then(|| env.tr.size(safe));
        let label = env.tr.t("view-recommendations");
        let selected = self.view == View::Recommendations;
        if sidebar_item(
            ui,
            env,
            Icon::ShieldCheckmark,
            &label,
            selected,
            true,
            chip.as_deref(),
        ) {
            self.view = View::Recommendations;
        }

        ui.add_space(10.0);
        widgets::section_label(ui, &pal, &env.tr.t("sidebar-types"));
        // Los 7 tipos y los 3 grises con significado, en DOS columnas: cabe en 1280 x 800 sin desplazarse.
        let mut entries: Vec<(egui::Color32, Option<egui::Color32>, String, Option<String>)> =
            FileType::ALL
                .iter()
                .map(|&t| {
                    (
                        pal.type_color(t),
                        None,
                        describe::type_name(env.tr, t),
                        None,
                    )
                })
                .collect();
        entries.push((
            pal.mixed(),
            Some(pal.type_color(FileType::Program)),
            env.tr.t("legend-mixed"),
            Some(env.tr.t("legend-mixed-tip")),
        ));
        entries.push((
            pal.info_only(),
            None,
            env.tr.t("legend-info-only"),
            Some(env.tr.t("legend-info-only-tip")),
        ));
        entries.push((
            pal.system_block(),
            Some(pal.system_border()),
            env.tr.t("legend-system"),
            Some(env.tr.t("legend-system-tip")),
        ));
        egui::Grid::new("tesela-legend")
            .num_columns(2)
            .spacing([6.0, 4.0])
            .min_col_width(96.0)
            .show(ui, |ui| {
                for (i, (fill, strip, text, tip)) in entries.iter().enumerate() {
                    legend_cell(ui, &pal, *fill, *strip, text, tip.as_deref());
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
    }

    fn status_bar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        let t = &self.tree;
        let info = &self.info;
        let ago = {
            let s = info.finished_at.elapsed().as_secs();
            if s < 60 {
                env.tr.t("ago-now")
            } else if s < 3600 {
                env.tr.ta("ago-minutes", args!["n" => s / 60])
            } else {
                env.tr.ta("ago-hours", args!["n" => s / 3600])
            }
        };
        let files = env.tr.int(info.files);
        let main = match (info.full_volume, info.volume) {
            (true, Some((total, free))) => env.tr.ta(
                "status-volume",
                args![
                    "used" => env.tr.size(total.saturating_sub(free)),
                    "total" => env.tr.size(total),
                    "files" => files,
                    "ago" => ago
                ],
            ),
            _ => env.tr.ta(
                "status-folder",
                args!["size" => env.tr.size(self.view().alloc(0)), "files" => files, "ago" => ago],
            ),
        };
        // Dos zonas que NUNCA se pisan: a la derecha "Reescanear" y el aviso de la demostración (se reservan primero), y a la
        // izquierda el resto, recortado al hueco que queda. Lo menos importante (el hueco sin explicar) va el último y se
        // trunca con puntos suspensivos; el texto completo está en su pista.
        let full = ui.available_rect_before_wrap();
        let right = ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(full)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                let r = ui.add(
                    egui::Label::new(
                        RichText::new(env.tr.t("rescan"))
                            .size(12.0)
                            .color(pal.accent),
                    )
                    .sense(Sense::click()),
                );
                r.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, env.tr.t("rescan"))
                });
                if r.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if r.has_focus() {
                    widgets::focus_ring(ui.painter(), &pal, r.rect.expand2(vec2(3.0, 1.0)), 4);
                }
                if r.clicked() {
                    env.actions.push(Action::Rescan);
                }
                if env.demo {
                    ui.label(
                        RichText::new(env.tr.t("demo-banner"))
                            .size(12.0)
                            .color(pal.review),
                    );
                }
            },
        );
        let left_rect = egui::Rect::from_min_max(
            full.min,
            egui::pos2(
                (right.response.rect.left() - 16.0).max(full.min.x),
                full.max.y,
            ),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(left_rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(left_rect));
                ui.label(RichText::new(main).size(12.0).color(pal.text_muted));
                if info.full_volume && info.volume.is_some() && !info.volume_scope {
                    // Con cuota y sin la capacidad real del volumen: las cifras son las de la cuenta, y no hay hueco sin explicar.
                    ui.label(
                        RichText::new(env.tr.t("drive-quota-scope"))
                            .size(12.0)
                            .color(pal.review),
                    );
                }
                if self.cleaned {
                    let tip = env.tr.t("cleaned-tip");
                    widgets::chip(ui, env, Some(Icon::Info), &env.tr.t("cleaned-badge"), pal.review, pal.accent_soft)
                        .on_hover_text(tip);
                }
                if env.dry_run {
                    ui.label(RichText::new(env.tr.t("dry-run-banner")).size(12.0).color(pal.review));
                }
                if info.approximate {
                    let tip = env.tr.t("approx-tip");
                    widgets::chip(ui, env, Some(Icon::Info), &env.tr.t("approx-badge"), pal.review, pal.accent_soft)
                        .on_hover_text(tip);
                }
                if info.cancelled {
                    ui.label(RichText::new(env.tr.t("status-stopped")).size(12.0).color(pal.review));
                }
                if info.dir_errors > 0 {
                    ui.label(
                        RichText::new(env.tr.ta("status-no-access", args!["n" => env.tr.int(info.dir_errors)]))
                            .size(12.0)
                            .color(pal.text_muted),
                    );
                }
                // Si el hueco supera el 5 %, "El mapa explica X de Y. ¿Y el resto?" (la última: si falta sitio, se trunca).
                if let (true, Some((total, free))) = (info.full_volume, info.volume) {
                    let used = total.saturating_sub(free);
                    let explained = t.stats().scanned_alloc;
                    if !info.cancelled && used > 0 && t.stats().unexplained_alloc * 100 > used * 5 {
                        let text = env.tr.ta(
                            "status-gap",
                            args!["explained" => env.tr.size(explained), "used" => env.tr.size(used)],
                        );
                        ui.add(egui::Label::new(RichText::new(text).size(12.0).color(pal.text_muted)).truncate())
                            .on_hover_text(env.tr.t("system-tip"));
                    }
                }
            },
        );
        ui.advance_cursor_after_rect(full);
    }
}

fn bar_frame(fill: egui::Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::NONE)
        .inner_margin(Margin::symmetric(16, 0))
}

fn legend_cell(
    ui: &mut Ui,
    pal: &Palette,
    fill: egui::Color32,
    strip: Option<egui::Color32>,
    text: &str,
    tip: Option<&str>,
) {
    let r = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(3), fill);
        if let Some(s) = strip {
            let r =
                egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 3.0), rect.max);
            ui.painter().rect_filled(r, CornerRadius::ZERO, s);
        }
        ui.label(RichText::new(text).size(12.0).color(pal.text));
    });
    if let Some(t) = tip {
        r.response.on_hover_text(t);
    }
}

/// ¿Se puede pulsar la fila de esta unidad en la barra lateral? Solo si NO es la unidad que ya se está explorando.
pub(crate) fn drive_row_is_interactive(selected: bool) -> bool {
    !selected
}

/// Elemento de la barra lateral. Devuelve `true` si se pulsó (y está habilitado).
fn sidebar_item(
    ui: &mut Ui,
    env: &mut Env,
    icon: Icon,
    label: &str,
    selected: bool,
    enabled: bool,
    chip: Option<&str>,
) -> bool {
    let pal = env.pal;
    let (rect, resp) = ui.allocate_exact_size(
        vec2(ui.available_width(), 36.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            pal.surface
        } else if enabled && resp.hovered() {
            pal.accent_soft
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
        if selected {
            let bar = egui::Rect::from_min_size(rect.min, vec2(3.0, rect.height()));
            ui.painter()
                .rect_filled(bar, CornerRadius::same(2), pal.accent);
        }
        let fg = if !enabled {
            theme::mix(pal.text_muted, pal.bg, 0.45)
        } else {
            pal.text
        };
        let icon_color = if selected {
            pal.accent
        } else if enabled {
            pal.text_muted
        } else {
            fg
        };
        env.icons.paint(
            ui.painter(),
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + 10.0, rect.center().y - 8.0),
                vec2(16.0, 16.0),
            ),
            icon,
            icon_color,
        );
        let font = if selected {
            theme::font_semibold(14.0)
        } else {
            egui::FontId::proportional(14.0)
        };
        ui.painter().text(
            egui::pos2(rect.left() + 36.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            fg,
        );
        if let Some(c) = chip {
            ui.painter().text(
                egui::pos2(rect.right() - 8.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                c,
                egui::FontId::proportional(12.0),
                pal.text_muted,
            );
        }
        if resp.has_focus() {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(2.0, pal.accent),
                egui::StrokeKind::Inside,
            );
        }
    }
    if !enabled {
        resp.on_hover_text(env.tr.t("next-version"));
        return false;
    }
    resp.clicked()
}

/// "Colorear: [Tipo] [Antigüedad] [Riesgo]". Antigüedad y Riesgo son de la v0.2: deshabilitados.
fn color_group(ui: &mut Ui, env: &mut Env) {
    let pal = env.pal;
    egui::Frame::new()
        .fill(pal.track)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // En una disposición de derecha a izquierda los elementos salen al revés: se añaden en orden inverso.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (key, enabled, selected) in [
                    ("color-risk", false, false),
                    ("color-age", false, false),
                    ("color-type", true, true),
                ] {
                    let text = env.tr.t(key);
                    let font = if selected {
                        theme::font_semibold(13.0)
                    } else {
                        egui::FontId::proportional(13.0)
                    };
                    let galley =
                        ui.painter()
                            .layout_no_wrap(text.clone(), font, egui::Color32::PLACEHOLDER);
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(galley.size().x + 24.0, 30.0),
                        if enabled {
                            Sense::click()
                        } else {
                            Sense::hover()
                        },
                    );
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::Button,
                            enabled,
                            selected,
                            text.clone(),
                        )
                    });
                    if selected {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(6), pal.surface);
                    }
                    if resp.has_focus() {
                        widgets::focus_ring(ui.painter(), &pal, rect, 6);
                    }
                    let col = if enabled {
                        pal.text
                    } else {
                        theme::mix(pal.text_muted, pal.track, 0.35)
                    };
                    ui.painter().galley(
                        egui::pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
                        galley,
                        col,
                    );
                    if !enabled {
                        resp.on_hover_text(env.tr.t("next-version-v02"));
                    }
                }
                ui.label(
                    RichText::new(env.tr.t("color-by"))
                        .size(13.0)
                        .color(pal.text_muted),
                );
            });
        });
}

/// Estilo especial de un nodo en las listas: icono y color de su clase.
pub(crate) fn node_icon(tree: &Tree, id: NodeId) -> Icon {
    match describe::special(tree, id) {
        Some(Special::SystemBlock) => Icon::Info,
        Some(Special::InfoOnly(_)) => Icon::Warning,
        None => {
            let n = tree.node(id);
            if n.flags & tesela_model::flags::CLOUD != 0 {
                Icon::Cloud
            } else if n.is_dir() {
                Icon::Folder
            } else {
                Icon::Document
            }
        }
    }
}
