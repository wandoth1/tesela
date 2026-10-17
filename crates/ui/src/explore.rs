//! Main window (PLAN.md §2.3): top bar with breadcrumb, search engine and "Colorize", side bar with
//! units, views and legend, the map or list, the details panel, the basket area (visible but empty and
//! inactive: is from increment 3) and the status bar.
//!
//! Map, list and details read the SAME state (`root`, `selection`) - changing the selection by one changes it
//! in the others.

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

/// Figures from the scan that are kept next to the tree.
#[derive(Clone, Debug)]
pub struct ScanInfo {
    pub req: ScanRequest,
    /// Scanned path as shown.
    pub display_root: String,
    /// The entire drive was scanned (there is system block and "space explained").
    pub full_volume: bool,
    /// (total, free) of the volume at the end.
    pub volume: Option<(u64, u64)>,
    /// `total` and `libre` are from VOLUME. `false` with a quota when Windows does not give the real capacity: they are the figures of
    /// the user's account and the status bar alerts them (same as the home card).
    pub volume_scope: bool,
    /// Stopped with "Stop and See": figures with "≥" are a minimum.
    pub cancelled: bool,
    /// Backup Mode Scanning (`FindFirstFileExW`) – No hard link deduplication.
    pub approximate: bool,
    /// Folders that could not be read (permissions, errors).
    pub dir_errors: u64,
    pub files: u64,
    pub elapsed: Duration,
    pub finished_at: Instant,
}

/// What is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sel {
    Node(NodeId),
    /// The "Others" group.
    Group(Others),
}

/// View of the central area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Map,
    List,
    Recommendations,
}

/// What can be done with an element regarding the basket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddState {
    /// It can be added.
    Addable,
    /// It is already in the basket (he or an ancestor).
    InBasket,
    /// Known folder: "Contents of..." only.
    ContentsOf,
    /// Cannot be selected as a cleanup destination (profile root, AppData...); its contents can still be cleaned.
    Untouchable,
    /// Completely denied.
    Denied,
    /// `pagefile.sys` and company: just inform.
    InfoOnly,
    /// The "Windows and other system data" block.
    SystemBlock,
    /// Cloud-only file: Cannot be selected to free up space.
    CloudOnly,
    /// An "Other" grouping: open and choose within.
    Group,
}

/// Exploring a tree.
pub struct Explore {
    pub(crate) tree: Arc<Tree>,
    /// What has already been cleaned, without rescanning (PLAN.md §3.6). `None` until the first cleaning.
    pub(crate) overlay: Option<Arc<Overlay>>,
    /// Something has already been cleaned up from the analysis: the histograms (types, age) are approximate until rescanning.
    pub(crate) cleaned: bool,
    pub(crate) info: ScanInfo,
    /// Version of the data for the layout cache (changes with the tree; in section 3, with the overlay).
    pub(crate) version: u64,
    /// Scripts used by names in the tree (to load the necessary fallback fonts).
    pub(crate) scripts: crate::fonts::Scripts,
    pub(crate) root: Root,
    /// Previous roots when opening nested "Other" groupings (Backspace returns for them).
    pub(crate) group_stack: Vec<Root>,
    /// The content of the sidebar does not fit and is scrolled (its bar is shown).
    pub(crate) sidebar_overflow: bool,
    /// The details panel does not fit and is scrolling (with its bar visible).
    pub(crate) details_overflow: bool,
    pub(crate) selection: Option<Sel>,
    /// Multiple selection: what is marked to be added next to the basket (children of the current folder; see `multisel`).
    pub(crate) multi: crate::multisel::Marks,
    /// The (folded) search with which the marks were last adjusted.
    pub(crate) marks_search: String,
    /// Marks that were dropped by a navigation and have not yet been warned (the warning appears in the next frame).
    pub(crate) marks_dropped: usize,
    /// Estimation of the basket (entries, bytes) with the version of the basket and the tree with which it was calculated.
    pub(crate) estimate: Option<((u64, u64), (usize, u64))>,
    /// Sibling index of the `Locator` of the "View" window while it is open (with tree and basket versions
    /// with which it was built): each frame only locates the rows that are seen, without rebuilding the indexes.
    pub(crate) locator_cache: Option<((u64, u64), crate::treepath::Index)>,
    /// How many times the estimate was recalculated (tests verify that it is not once per frame).
    pub(crate) estimate_computes: u32,
    pub(crate) view: View,
    /// The cleanup flow: basket, confirmation, execution and summary.
    pub(crate) flow: CleanFlow,
    /// Calculated recommendations (recalculated when changing the tree or open browsers).
    pub(crate) recs: Option<(u64, Recs)>,
    /// Safe is already preselected when opening Recommendations for the first time.
    pub(crate) recs_preselected: bool,
    pub(crate) basket_open: bool,
    pub(crate) browsers_open: Vec<Browser>,
    pub(crate) protection: Arc<Protection>,
    pub(crate) now_unix: u32,
    pub(crate) search: String,
    pub(crate) map: MapState,
    pub(crate) scroll_to_selection: bool,
    /// Row in the list to which keyboard focus should be passed on the next pass.
    pub(crate) list_focus: Option<NodeId>,
    pub(crate) animate: bool,
    /// Element on which the context menu was opened.
    pub(crate) ctx_target: Option<Sel>,
    pub(crate) toast: Option<(String, f64)>,
    /// Request to open the Explorer in progress (never blocks: it is consulted in each frame).
    reveal: Option<crate::platform::RevealHandle>,
    focus_search: bool,
    /// The search engine has keyboard focus (the navigation keys do not act while typing).
    search_focused: bool,
    /// Last navigation widget (the map or a list row) that had focus. Enter, Backspace and arrows
    /// They only act if the focus is on it or on no widget: with another focused control (a button, a link...) the
    /// keys are yours.
    pub(crate) nav_focus: Option<egui::Id>,
    search_hits: Option<(String, Root, Arc<Vec<NodeId>>)>,
    /// The same successes as a set (for the map glazing: membership in O(1)).
    search_set: Option<(String, Root, Arc<std::collections::HashSet<NodeId>>)>,
}

/// The children being displayed (the entire folder or an open "Other" grouping).
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

/// Number that identifies the tree in the cache (one counter per process).
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

    /// From the result of a real scan.
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

    /// From the synthetic demo tree.
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

    /// Like [`Explore::demo`] with a given tree (scaling tests use one with thousands of children).
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

    /// Set the demo scan based on `start` (folders by name; no animations).
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

    /// Places the demo scan on a cleanup flow screen (captures and tests; backup is
    /// the simulated one: it does not touch the disc).
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
                // The example basket: how safe it can be cleaned now, a project folder and a file of yours.
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

    /// It moves to cleaning the caches of open browsers that are not cleaned and how much would be freed by closing them.
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

    /// Name of what was explored (for the window title).
    pub fn title(&self) -> String {
        self.info.req.label.clone()
    }

    // --- Navigation ---------------------------------------------------------------------------------------------

    /// Can I go back to the home screen? Not while a cleanup (such as closing the window) is running.
    pub(crate) fn can_go_home(&self) -> bool {
        !self.flow.is_running()
    }

    /// Can you go up a level?
    pub fn can_up(&self) -> bool {
        self.root.is_group() || self.root.node != 0
    }

    /// Go up one level (from an "Other" grouping you return to your folder). The folder being exited is selected.
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
        // The marked speaks of the folder that is left: it is not dragged to the new one.
        self.drop_marks_for_navigation();
        self.scroll_to_selection = true;
    }

    /// Enter an item: a folder (which becomes the root of the map) or an "Other" grouping (which opens).
    /// It doesn't do anything with a file. Returns whether the root changed.
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

    /// Jump to any folder (breadcrumb).
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

    /// Navigate releases what is marked (it speaks of the folder that is left) and notes it to warn you.
    fn drop_marks_for_navigation(&mut self) {
        self.marks_dropped += self.multi.len();
        self.multi.clear();
    }

    /// Select (or clear selection).
    pub fn select(&mut self, sel: Option<Sel>) {
        self.selection = sel;
    }

    /// The tree seen through the cleaning overlay.
    pub(crate) fn view(&self) -> TreeView<'_> {
        TreeView::new(&self.tree, self.overlay.as_deref())
    }

    /// Children (visible) that the map or list is now displaying.
    pub(crate) fn shown_children(&self) -> Shown {
        let kids = tesela_layout::Children::of_view(&self.view(), self.root.node);
        let end = (self.root.end as usize).min(kids.len());
        let start = (self.root.start as usize).min(end);
        Shown { kids, start, end }
    }

    /// Applies to the tree what really disappeared (overlay, PLAN.md §3.6), without rescanning: what was deleted in its entirety
    /// It is no longer visible and what was only partially cleaned shrinks. The ancestors subtract the same.
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
        // The layout cache and mesh are invalidated by version; selection and current folder are kept
        // valid.
        self.version = next_version();
        self.search_hits = None;
        self.search_set = None;
        let view = TreeView::new(&self.tree, self.overlay.as_deref());
        // What no longer exists is no longer marked (the rest is preserved).
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
            // An open "Other" grouping no longer describes the same thing: you go back to the folder.
            self.root = Root::node(node);
            self.group_stack.clear();
            self.multi.clear();
        }
    }

    /// Text of the path of an element (only for display or copy).
    pub(crate) fn path_of(&self, sel: Sel) -> String {
        match sel {
            Sel::Node(id) => self.tree.path_lossy(id),
            Sel::Group(o) => self.tree.path_lossy(o.parent),
        }
    }

    /// Nodes in the current folder whose name contains the search (case-insensitive), or `None` without search.
    /// The search hits as a set, or `None` without search. It is recalculated only if the search or root changes.
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

    // --- Painted --------------------------------------------------------------------------------------------------

    /// Paint the main window.
    pub fn show(&mut self, ui: &mut Ui, env: &mut Env) {
        let ctx = ui.ctx().clone();
        // The cleanup flow moves forward (approval and execution threads) and what disappeared is applied to the tree.
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
        // The summary takes up the entire window.
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

        // Order: what is added first is left out the most.
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
        // The search could change in the top bar: the marked that is no longer a success is no longer so.
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
                // If the content does not fit (small window) the scroll bar is SOLID and always visible: nothing remains
                // cut off without warning.
                ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                let out = egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.sidebar(ui, env));
                self.sidebar_overflow = out.content_size.y > out.inner_rect.height() + 0.5;
            });
        // Recommendations fill the entire width (as in the mockup): no details panel.
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
        // The selection bar always goes (same height) and only in the central column, behind the side panels: yes
        // If it appeared only when marking, the map would be repositioned under the pointer, and removing the height of the sidebar would make it overflow.
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
        // "Open in Explorer" result: the background thread replies to the caller; an unresponsive shell or a
        // error come out as a warning.
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
        // Browse dropped marks: a short notice (no other more recent notice from the navigation itself is stepped on).
        if self.marks_dropped > 0 {
            let n = std::mem::take(&mut self.marks_dropped) as u64;
            let msg = env.tr.ta("sel-dropped", args!["count" => env.tr.int(n)]);
            self.toast = Some((msg, ctx.input(|i| i.time) + 3.0));
        }
        // Temporary notice ("Copied Route"): above the basket bar and, if present, the selection bar.
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

    /// Open Explorer with `path` selected WITHOUT locking the interface. If there is already an unfinished request or
    /// reached the limit of threads, warn with a toast instead of launching another one.
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
        // With a modal window open ("About") NO exploration shortcut works, neither does Ctrl+F or F5.
        if env.modal_open || self.flow.modal_open() {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            self.focus_search = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F5)) {
            env.actions.push(Action::Rescan);
        }
        // Alt+Home: to the home screen (does not clash with Home itself, which goes to the first row of the list: here the
        // shortcut requires Alt). With a cleaning in progress it does nothing, just like the button.
        if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, Key::Home)) && self.can_go_home() {
            env.actions.push(Action::GoHome);
        }
        if self.search_focused {
            return;
        }
        // An open menu stays with the keyboard (Escape closes it, Enter activates its option).
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        // Enter, Backspace and arrows are navigation ONLY if the focus is on the map, in a list row
        // or in any widget. With a button in focus, Enter and Space activate it (they are not consumed here).
        let nav = match ctx.memory(|m| m.focused()) {
            None => true,
            Some(id) => self.nav_focus == Some(id),
        };
        if nav && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) {
            self.up();
        }
        // Ctrl+A: mark everything that is seen (the hits if there is a search). The search engine has its own (it already came up).
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
            // First the marked is emptied, then the search and finally the active element.
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
            // Home: the "Which drive do you want to review?" screen (with "Analyze a folder..."). To the left of "Up".
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
                // Seeker.
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
        // Chain of nodes from root to current folder.
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
            // Hover Background Hole: Filled when it is known whether the row is under the pointer (the
            // response is calculated AFTER drawing the content) and falls below it.
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
                    // The current unit is not clickable: the screen reader does not announce it as an enabled button.
                    drive_row_is_interactive(selected),
                    selected,
                    env.tr.ta("analyze-drive", args!["name" => title.clone()]),
                )
            });
            // Only units that are NOT the current one can be tapped: they light up when you hover the pointer (such as
            // the sidebar), they change the cursor and explain what they do. The current one has no "hover over" state.
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
            // `clicked` is also triggered by Enter and Space when the row has focus.
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
        // Recommendations, with how safe it can be released as a chip.
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
        // The 7 types and the 3 grays with meaning, in TWO columns: fits in 1280 x 800 without scrolling.
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
        // Two areas that are NEVER stepped on: on the right "Rescan" and the demo notice (they are reserved first), and on the
        // left the rest, cut to the gap that remains. The least important thing (the unexplained gap) comes last and is
        // truncated with ellipses; the full text is on your track.
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
                    // With a quota and without the real capacity of the volume: the figures are those of the account, and there is no unexplained gap.
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
                // If the gap exceeds 5%, "The map explains X of Y. What about the rest?" (the last one: if there is no space, it is truncated).
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

/// Can I click this unit's row in the sidebar? Only if it is NOT the drive already being scanned.
pub(crate) fn drive_row_is_interactive(selected: bool) -> bool {
    !selected
}

/// Sidebar element. Returns `true` if pressed (and enabled).
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

/// "Coloring: [Type] [Age] [Risk]." Age and Risk are from v0.2: disabled.
fn color_group(ui: &mut Ui, env: &mut Env) {
    let pal = env.pal;
    egui::Frame::new()
        .fill(pal.track)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // In a right-to-left arrangement the elements come out the other way around: they are added in reverse order.
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

/// Special style of a node in lists: icon and color of its class.
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
