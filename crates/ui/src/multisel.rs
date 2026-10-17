//! Multiple selection: mark several items in the current folder and add them to the basket together.
//!
//! * Marks are stored in [`Marks`] and are independent of `Explore::selection` (the active item shown in Details).
//! * Only children of the current folder or the open "Others" group can be marked. If a click or key lands on a deeper
//!   descendant (a hinted map cell), its ancestor in the current folder is marked. Siblings are never nested, so their
//!   sizes cannot be counted twice. Basket membership is the only possible overlap; `add_state` excludes and explains it
//!   with `InBasket`.
//! * Marks are never hidden: changing folders (entering one, going up, using breadcrumbs, or opening "Other") clears them.
//!   Changing the search keeps only marks that still match, so "Add N" never adds items absent from the list. Rescanning
//!   or returning Home clears all marks; cleanup removes only items that actually disappeared. A brief prompt reports any
//!   marks dropped while browsing.
//! * Each item is checked with `add_state` before batch insertion through `Basket::add_batch`, which also checks
//!   `add_path` and `add_contents_of`. No protection is bypassed. For one item, the menu and Details panel use
//!   `Explore::basket_add_one`.

use crate::app::Env;
use crate::args;
use crate::explore::{AddState, Explore, Sel};
use crate::i18n::Tr;
use crate::icons::Icon;
use crate::theme;
use crate::widgets::{self, ButtonStyle};
use egui::{
    Align, Color32, CornerRadius, Layout, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    WidgetInfo, WidgetType, pos2, vec2,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tesela_cleaner::{AddOutcome, BatchKind};
use tesela_layout::Others;
use tesela_model::NodeId;

/// Height of the selection bar (two lines of text).
pub(crate) const BAR_H: f32 = 44.0;
/// Width occupied by the column of boxes in the list.
pub(crate) const CHECK_W: f32 = 26.0;

/// The marked and the "Select" mode.
#[derive(Default)]
pub(crate) struct Marks {
    set: HashSet<NodeId>,
    /// Last element touched (the fixed end of a range with Shift).
    pub(crate) anchor: Option<NodeId>,
    /// "Select" mode: clicking an item checks or unchecks it, and the list shows its boxes.
    pub(crate) mode: bool,
    /// Changes with each modification (summary cache key).
    rev: u64,
    cache: Option<(u64, Arc<Summary>)>,
    /// The marks before a burst of Shift+arrows: what is marked is that plus the range, so the range also shrinks.
    run_base: Option<HashSet<NodeId>>,
    /// What is each mark regarding the basket (`add_state`), valid as long as the tree or the basket do not change: mark or
    /// unchecking one only classifies that one, not the others.
    cls_key: (u64, u64),
    cls: HashMap<NodeId, AddState>,
}

impl Marks {
    pub(crate) fn len(&self) -> usize {
        self.set.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub(crate) fn contains(&self, id: NodeId) -> bool {
        self.set.contains(&id)
    }

    /// Those marked, in order of identifier.
    pub(crate) fn sorted(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.set.iter().copied().collect();
        v.sort_unstable();
        v
    }

    /// Empties the marked (the "Select" mode remains as is).
    pub(crate) fn clear(&mut self) {
        self.run_base = None;
        if !self.set.is_empty() {
            self.set.clear();
            self.rev += 1;
        }
        self.anchor = None;
    }

    /// Check `id` if it was not and uncheck it if it was. It becomes the anchor of the ranks.
    pub(crate) fn toggle(&mut self, id: NodeId) {
        self.run_base = None;
        if !self.set.remove(&id) {
            self.set.insert(id);
        }
        self.anchor = Some(id);
        self.rev += 1;
    }

    pub(crate) fn add(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.run_base = None;
        self.set.extend(ids);
        self.rev += 1;
    }

    pub(crate) fn remove_all(&mut self, ids: &[NodeId]) {
        self.run_base = None;
        for id in ids {
            self.set.remove(id);
        }
        self.rev += 1;
    }

    /// Check what was not checked between `ids` and uncheck what was.
    pub(crate) fn invert(&mut self, ids: &[NodeId]) {
        self.run_base = None;
        for &id in ids {
            if !self.set.remove(&id) {
                self.set.insert(id);
            }
        }
        self.rev += 1;
    }

    /// Keep only what complies with `keep` (after cleaning: what no longer exists is no longer marked).
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(NodeId) -> bool) {
        self.run_base = None;
        let before = self.set.len();
        self.set.retain(|&id| keep(id));
        if self.set.len() != before {
            self.rev += 1;
        }
    }

    /// The active element was moved without Shift: it is the new anchor and ends the Shift+arrow burst.
    pub(crate) fn plain_move(&mut self, id: NodeId) {
        self.anchor = Some(id);
        self.run_base = None;
    }

    /// Shift+arrows: what is marked becomes what was before the burst plus `range` (the range between the anchor and the end).
    pub(crate) fn set_run(&mut self, range: &[NodeId]) {
        let base = match self.run_base.take() {
            Some(b) => b,
            None => self.set.clone(),
        };
        self.set = base.iter().copied().chain(range.iter().copied()).collect();
        self.run_base = Some(base);
        self.rev += 1;
    }
}

/// Should a `clicked()` WITHOUT a pointer toggle the marking of a row? Yes if it is the default click of a screen reader
/// (no keys) or the first press of Space; not with Enter (that's "enter") or with Space auto-repeat.
pub(crate) fn keyboard_click_toggles(i: &egui::InputState) -> bool {
    let (mut first, mut repeat, mut enter) = (false, false, false);
    for e in &i.events {
        if let egui::Event::Key {
            key,
            pressed: true,
            repeat: r,
            ..
        } = e
        {
            match key {
                egui::Key::Enter => enter = true,
                egui::Key::Space if *r => repeat = true,
                egui::Key::Space => first = true,
                _ => {}
            }
        }
    }
    !enter && (first || !repeat)
}

/// Was Space pressed (the press, not the auto-repeat hold)?
pub(crate) fn space_pressed_once(i: &egui::InputState) -> bool {
    i.events.iter().any(|e| {
        matches!(
            e,
            egui::Event::Key {
                key: egui::Key::Space,
                pressed: true,
                repeat: false,
                ..
            }
        )
    })
}

/// Elements of `order` between `a` and `b`, both inclusive and in either direction. Empty if any are not in `order`.
pub(crate) fn range_between(order: &[NodeId], a: NodeId, b: NodeId) -> &[NodeId] {
    let (Some(ia), Some(ib)) = (
        order.iter().position(|&x| x == a),
        order.iter().position(|&x| x == b),
    ) else {
        return &[];
    };
    &order[ia.min(ib)..=ia.max(ib)]
}

/// What would happen to the marked item if it were added to the basket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// How many items are marked.
    pub marked: usize,
    /// Those that CAN can be added (in order of identifier).
    pub add: Vec<NodeId>,
    /// Of them, the known folders from which the content is added and not the folder.
    pub contents_of: HashSet<NodeId>,
    /// What those who are added occupy (each brother only once).
    pub bytes: u64,
    /// Some of those added have an incomplete figure (minimum).
    pub lower: bool,
    pub in_basket: usize,
    pub protected: usize,
    pub info: usize,
    pub cloud: usize,
}

impl Summary {
    /// How many of those marked CANNOT be added.
    pub(crate) fn cannot(&self) -> usize {
        self.in_basket + self.protected + self.info + self.cloud
    }

    /// How many of those added are known folders (their contents are added).
    pub(crate) fn contents(&self) -> usize {
        self.contents_of.len()
    }
}

/// The two lines of the bar: the count with the size and, if necessary, what cannot be added and why.
pub(crate) fn summary_lines(tr: &Tr, s: &Summary) -> (String, Option<String>) {
    let n = s.marked as u64;
    let count = tr.int(n);
    let first = if s.add.is_empty() {
        tr.ta("sel-count-only", args!["n" => n, "count" => count])
    } else if s.cannot() > 0 {
        // With something excluded, the size is only that of what would be added.
        let size = if s.lower {
            tr.size_min(s.bytes)
        } else {
            tr.size(s.bytes)
        };
        tr.ta(
            "sel-count-partial",
            args!["n" => n, "count" => count, "size" => size],
        )
    } else {
        let size = if s.lower {
            tr.size_min(s.bytes)
        } else {
            tr.size(s.bytes)
        };
        tr.ta(
            "sel-count",
            args!["n" => n, "count" => count, "size" => size],
        )
    };
    let mut parts: Vec<String> = Vec::new();
    let cannot = s.cannot();
    if cannot > 0 {
        let why: Vec<String> = [
            ("sel-why-protected", s.protected),
            ("sel-why-in-basket", s.in_basket),
            ("sel-why-info", s.info),
            ("sel-why-cloud", s.cloud),
        ]
        .iter()
        .filter(|(_, k)| *k > 0)
        .map(|(key, k)| tr.ta(key, args!["n" => *k as u64, "count" => tr.int(*k as u64)]))
        .collect();
        parts.push(tr.ta(
            "sel-cannot",
            args!["n" => cannot as u64, "count" => tr.int(cannot as u64), "detail" => why.join(", ")],
        ));
    }
    if s.contents() > 0 {
        let c = s.contents() as u64;
        parts.push(tr.ta("sel-contents", args!["n" => c, "count" => tr.int(c)]));
    }
    let second = (!parts.is_empty()).then(|| parts.join(" · "));
    (first, second)
}

impl Explore {
    /// The items the folder now displays, in list order: hits if there is a search, and if not, all
    /// the children (or those of the open "Others" group).
    pub(crate) fn visible_order(&mut self) -> Vec<NodeId> {
        match self.search_matches() {
            Some(h) => h.to_vec(),
            None => self.shown_children().iter().collect(),
        }
    }

    /// The elements grouped by an "Other" cell.
    pub(crate) fn others_members(&self, o: Others) -> Vec<NodeId> {
        let kids = tesela_layout::Children::of_view(&self.view(), o.parent);
        let end = ((o.start + o.count) as usize).min(kids.len());
        ((o.start as usize).min(end)..end)
            .map(|i| kids.get(i))
            .collect()
    }

    /// Are all items in that grouping marked "Other"?
    pub(crate) fn others_all_marked(&self, o: Others) -> bool {
        let m = self.others_members(o);
        !m.is_empty() && m.iter().all(|&id| self.multi.contains(id))
    }

    /// The child of the current folder that is `id` or contains it (a hinted map cell or a deeper selection
    /// are marked by their ancestry from this folder). `None` if `id` does not hang on what is being viewed.
    pub(crate) fn child_of_root(&self, id: NodeId) -> Option<NodeId> {
        let mut cur = id;
        loop {
            let p = self.tree.parent(cur)?;
            if p == self.root.node {
                break;
            }
            cur = p;
        }
        if self.root.is_group() && !self.shown_children().iter().any(|c| c == cur) {
            return None;
        }
        Some(cur)
    }

    /// Can the `id` flag be toggled? It has to hang on what is seen and, with an active search, it either be a success or
    /// be already marked (it is always possible to uncheck): this way, what the list does not show is never marked.
    pub(crate) fn can_toggle_mark(&mut self, id: NodeId) -> bool {
        let Some(c) = self.child_of_root(id) else {
            return false;
        };
        self.multi.contains(c) || self.search_set().is_none_or(|hits| hits.contains(&c))
    }

    /// Checks or unchecks an item (an "Other" grouping checks or unchecks all of its items).
    pub(crate) fn marks_toggle(&mut self, sel: Sel) {
        match sel {
            Sel::Node(id) => {
                if self.can_toggle_mark(id)
                    && let Some(c) = self.child_of_root(id)
                {
                    self.multi.toggle(c);
                }
            }
            Sel::Group(o) => {
                let mut members = self.others_members(o);
                // With search, only the elements of the group that are hits (what is not seen is never marked).
                if let Some(hits) = self.search_matches() {
                    let hits: HashSet<NodeId> = hits.iter().copied().collect();
                    members.retain(|id| hits.contains(id));
                }
                if members.is_empty() {
                    return;
                }
                if members.iter().all(|&id| self.multi.contains(id)) {
                    self.multi.remove_all(&members);
                } else {
                    self.multi.add(members);
                }
            }
        }
    }

    /// Marks the range between the anchor and `target` (in `order` order of the list), without removing what was already marked.
    pub(crate) fn marks_range_in(&mut self, order: &[NodeId], target: NodeId) {
        let anchor = self
            .multi
            .anchor
            .filter(|a| order.contains(a))
            .unwrap_or(target);
        let range = range_between(order, anchor, target).to_vec();
        self.multi.add(range);
        self.multi.anchor = Some(anchor);
    }

    /// Shift+arrows: what is marked is what was before the burst plus the range between the anchor and `target` (it also shrinks).
    pub(crate) fn marks_shift_extend(&mut self, order: &[NodeId], target: NodeId) {
        let anchor = self
            .multi
            .anchor
            .filter(|a| order.contains(a))
            .unwrap_or(target);
        let range = range_between(order, anchor, target).to_vec();
        self.multi.set_run(&range);
        self.multi.anchor = Some(anchor);
    }

    /// When changing the search, only the marks that follow among the hits are kept: so what "Add N" adds is
    /// always something that is seen. (If the search is deleted, nothing comes back: the marks already removed remain removed.)
    pub(crate) fn sync_marks_with_search(&mut self) {
        let q = crate::describe::fold(self.search.trim());
        if q == self.marks_search {
            return;
        }
        self.marks_search.clone_from(&q);
        if q.is_empty() || self.multi.is_empty() {
            return;
        }
        if let Some(hits) = self.search_matches() {
            let hits: HashSet<NodeId> = hits.iter().copied().collect();
            self.multi.retain(|id| hits.contains(&id));
        }
    }

    /// "Select all here": everything that is seen (the hits if there is a search).
    pub(crate) fn marks_all_here(&mut self) {
        let ids = self.visible_order();
        self.multi.add(ids);
    }

    /// "Invert selection" on what is seen: what is checked is unchecked and the rest is checked.
    pub(crate) fn marks_invert_here(&mut self) {
        let ids = self.visible_order();
        self.multi.invert(&ids);
    }

    /// What would happen to what was marked when added, classified with the same rules as the basket icon (`add_state`).
    /// No cache: uses Add (counts current state) and tests.
    pub(crate) fn compute_summary(&self) -> Summary {
        self.summarize(&mut HashMap::new())
    }

    fn summarize(&self, cls: &mut HashMap<NodeId, AddState>) -> Summary {
        let view = self.view();
        let mut s = Summary::default();
        for id in self.multi.sorted() {
            s.marked += 1;
            let state = *cls
                .entry(id)
                .or_insert_with(|| self.add_state(Sel::Node(id)));
            match state {
                AddState::Addable | AddState::ContentsOf => {
                    if state == AddState::ContentsOf {
                        s.contents_of.insert(id);
                    }
                    s.add.push(id);
                    s.bytes += view.alloc(id);
                    s.lower |= crate::describe::is_lower_bound(&self.tree, id);
                }
                AddState::InBasket => s.in_basket += 1,
                AddState::Untouchable | AddState::Denied => s.protected += 1,
                AddState::InfoOnly | AddState::SystemBlock => s.info += 1,
                AddState::CloudOnly => s.cloud += 1,
                AddState::Group => {}
            }
        }
        s
    }

    /// The summary of what was marked, recalculated only if you changed the selection, tree or basket (versions, without formatting the
    /// basket). The classification of each marking is preserved as long as the tree or basket does not change: mark or
    /// unchecking one does not reclassify the others.
    pub(crate) fn marks_summary(&mut self) -> Arc<Summary> {
        if self.multi.is_empty() {
            return Arc::default();
        }
        let key = (self.version, self.flow.basket.rev());
        let stamp = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (self.multi.rev, key).hash(&mut h);
            h.finish()
        };
        if let Some((k, s)) = &self.multi.cache
            && *k == stamp
        {
            return s.clone();
        }
        let mut cls = std::mem::take(&mut self.multi.cls);
        if self.multi.cls_key != key {
            cls.clear();
            self.multi.cls_key = key;
        }
        let s = Arc::new(self.summarize(&mut cls));
        self.multi.cls = cls;
        self.multi.cache = Some((stamp, s.clone()));
        s
    }

    /// Add the marked items that can be added to the basket, in batches, and empty the selection. Returns (those who entered, those
    /// marked). Each element goes through `add_state` and `Basket` checks (local drive path,
    /// protection, already covered), the same as the basket icon.
    pub(crate) fn marks_add_core(&mut self) -> (usize, usize) {
        // It is recalculated at the moment (not with the cache): what counts is the state now.
        let s = self.compute_summary();
        let items: Vec<(std::path::PathBuf, BatchKind)> = s
            .add
            .iter()
            .map(|&id| {
                let kind = if s.contents_of.contains(&id) {
                    BatchKind::ContentsOf
                } else {
                    BatchKind::Path
                };
                (self.tree.path(id), kind)
            })
            .collect();
        let prot = self.protection.clone();
        let out = self.flow.basket.add_batch(items, &prot);
        let added = out
            .iter()
            .filter(|o| matches!(o, AddOutcome::Added { .. }))
            .count();
        self.multi.clear();
        (added, s.marked)
    }

    /// "Add N to Cart" bar: like [`Explore::marks_add_core`], with prompt. Does nothing while there is a
    /// cleaning in progress. Returns how many entered.
    pub(crate) fn marks_add_to_basket(&mut self, tr: &Tr, now: f64) -> usize {
        if self.flow.busy() {
            return 0;
        }
        let (added, marked) = self.marks_add_core();
        let mut msg = tr.ta(
            "sel-added",
            args!["n" => added as u64, "count" => tr.int(added as u64)],
        );
        let skipped = marked - added;
        if skipped > 0 {
            msg.push_str(" · ");
            msg.push_str(&tr.ta(
                "sel-added-skipped",
                args!["n" => skipped as u64, "count" => tr.int(skipped as u64)],
            ));
        }
        self.toast = Some((msg, now + 3.0));
        added
    }

    /// The selection bar (below the map or list): the "Select" switch and, with something checked, the count, which
    /// which cannot be added and buttons. It always occupies the same height: this way the map is not repositioned when marking the first one.
    /// Controls are created in reading order (Switch, Add, Uncheck) so Tab cycles through them like this.
    pub(crate) fn selection_bar(&mut self, ui: &mut Ui, env: &mut Env) {
        let pal = env.pal;
        let tr = env.tr;
        let summary = self.marks_summary();
        let has = !self.multi.is_empty();
        let busy = self.flow.busy();
        let mut do_add = false;
        let mut do_clear = false;
        let mut refocus = false;
        let full = ui.available_rect_before_wrap();
        // The bar occupies the central column: the text of the controls is shortened depending on its width (the accessible name is not
        // changes). Buttons lose text below 600px; the switch shows it as much as it fits next to the
        // space occupied by the buttons (about 350 px with text, 90 without it) and about 210 px of summary text.
        let button_text = full.width() >= 600.0;
        let toggle_text = full.width() - if button_text { 352.0 } else { 92.0 } >= 338.0;
        let clear = tr.t("sel-clear");
        let can = !summary.add.is_empty() && !busy;
        let add_label = if summary.add.is_empty() {
            tr.t("sel-add-none")
        } else {
            let n = summary.add.len() as u64;
            tr.ta("sel-add", args!["n" => n, "count" => tr.int(n)])
        };
        // Width of the button area (measured the same as `widgets::button`) to distribute the space before drawing.
        let right_w = if !has {
            0.0
        } else if button_text {
            text_button_w(ui, &clear, false) + text_button_w(ui, &add_label, true) + 8.0 + 2.0
        } else {
            36.0 + 36.0 + 8.0 + 2.0
        };
        let left_rect = if has {
            Rect::from_min_max(
                full.min,
                pos2((full.right() - right_w - 12.0).max(full.min.x), full.max.y),
            )
        } else {
            full
        };
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(left_rect)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(left_rect));
                ui.spacing_mut().item_spacing.x = 10.0;
                self.mode_toggle(ui, env, toggle_text);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if has {
                        let (first, second) = summary_lines(tr, &summary);
                        // `vertical` places the block at the top: it is centered by eye (one or two lines) on the bar.
                        ui.add_space(if second.is_some() { 5.0 } else { 13.0 });
                        let full_text = match &second {
                            Some(s) => format!("{first} · {s}"),
                            None => first.clone(),
                        };
                        let l = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&first)
                                    .font(theme::font_semibold(14.0))
                                    .color(pal.text),
                            )
                            .truncate(),
                        );
                        l.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &first));
                        l.on_hover_text(&full_text);
                        if let Some(sec) = second {
                            let l2 = ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&sec).size(12.0).color(pal.text_muted),
                                )
                                .truncate(),
                            );
                            l2.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &sec));
                            l2.on_hover_text(full_text);
                        }
                    } else {
                        let key = if self.multi.mode {
                            "sel-hint-mode"
                        } else {
                            "sel-hint"
                        };
                        let text = tr.t(key);
                        ui.add_space(13.0);
                        let l = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&text).size(12.0).color(pal.text_muted),
                            )
                            .truncate(),
                        );
                        l.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
                        // The track is cut into narrow windows: complete, in view of the mouse.
                        l.on_hover_text(format!("{text} · {}", tr.t("sel-keys-tip")));
                    }
                });
            },
        );
        if has {
            let right_rect = Rect::from_min_max(pos2(full.right() - right_w, full.top()), full.max);
            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(right_rect)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let r = if button_text {
                        widgets::button(
                            ui,
                            env,
                            ButtonStyle::Secondary,
                            Some(Icon::Cart),
                            &add_label,
                            can,
                        )
                    } else {
                        widgets::icon_button(ui, env, Icon::Cart, &add_label, can)
                    };
                    if !can {
                        // With nothing to add, or with a cleaning in progress: the clue tells which of the two.
                        let why = if busy {
                            "sel-add-busy-tip"
                        } else {
                            "sel-add-none-tip"
                        };
                        r.clone().on_hover_text(tr.t(why));
                    }
                    if r.clicked() {
                        do_add = true;
                        refocus |= r.interact_pointer_pos().is_none();
                    }
                    let r = if button_text {
                        widgets::button(ui, env, ButtonStyle::Ghost, None, &clear, true)
                    } else {
                        widgets::icon_button(ui, env, Icon::Dismiss, &clear, true)
                    };
                    if r.clicked() {
                        do_clear = true;
                        refocus |= r.interact_pointer_pos().is_none();
                    }
                },
            );
        }
        ui.advance_cursor_after_rect(full);
        if do_clear {
            self.multi.clear();
        }
        if do_add {
            let now = ui.input(|i| i.time);
            self.marks_add_to_basket(tr, now);
        }
        // With a keyboard, the buttons disappear when you press them: the focus goes to the switch instead of being lost.
        if refocus {
            ui.memory_mut(|m| m.request_focus(egui::Id::new(TOGGLE_ID)));
        }
    }

    /// The "Select" switch (selection mode): a box with your text, reachable with Tab and accessible as a box.
    fn mode_toggle(&mut self, ui: &mut Ui, env: &mut Env, with_text: bool) {
        let pal = env.pal;
        let label = env.tr.t("sel-toggle");
        let on = self.multi.mode;
        let galley = ui.painter().layout_no_wrap(
            label.clone(),
            theme::font_semibold(14.0),
            Color32::PLACEHOLDER,
        );
        // No text, a 34px square with just the checkbox (the accessible name and hint still say "Select").
        let w = if with_text {
            12.0 + 16.0 + 8.0 + galley.size().x + 14.0
        } else {
            34.0
        };
        let (rect, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
        let resp = ui.interact(rect, egui::Id::new(TOGGLE_ID), Sense::click());
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, on, label.clone()));
        if ui.is_rect_visible(rect) {
            let fill = if on || resp.hovered() {
                pal.accent_soft
            } else {
                pal.surface
            };
            ui.painter().rect(
                rect,
                CornerRadius::same(8),
                fill,
                Stroke::new(1.0, if on { pal.accent } else { pal.border }),
                StrokeKind::Inside,
            );
            let cx = if with_text {
                rect.left() + 20.0
            } else {
                rect.center().x
            };
            paint_checkbox(
                ui.painter(),
                env,
                Rect::from_center_size(pos2(cx, rect.center().y), vec2(16.0, 16.0)),
                on,
            );
            if with_text {
                ui.painter().galley(
                    pos2(
                        rect.left() + 12.0 + 16.0 + 8.0,
                        rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    pal.text,
                );
            }
            if resp.has_focus() {
                widgets::focus_ring(ui.painter(), &pal, rect.expand(1.0), 9);
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            self.multi.mode = !on;
        }
        let tip = format!(
            "{label} · {} {}",
            env.tr.t("sel-toggle-tip"),
            env.tr.t("sel-keys-tip")
        );
        resp.on_hover_text(tip);
    }
}

/// Identifier for the "Select" switch (to return focus after Adding or Unchecking with the keyboard).
const TOGGLE_ID: &str = "tesela-sel-toggle";

/// Width of a `widgets::button` text button (with 18 px icon and its gap, or without it).
fn text_button_w(ui: &Ui, text: &str, icon: bool) -> f32 {
    let g = ui.painter().layout_no_wrap(
        text.to_string(),
        theme::font_semibold(14.0),
        Color32::PLACEHOLDER,
    );
    (24.0 + if icon { 26.0 } else { 0.0 } + g.size().x).max(40.0)
}

/// 16 px box: marked with accent fill and a white check mark; unmarked, just the edge.
pub(crate) fn paint_checkbox(painter: &egui::Painter, env: &mut Env, rect: Rect, on: bool) {
    let pal = env.pal;
    if on {
        painter.rect_filled(rect, CornerRadius::same(4), pal.accent);
        env.icons
            .paint(painter, rect.shrink(2.0), Icon::Checkmark, Color32::WHITE);
    } else {
        painter.rect_stroke(
            rect.shrink(0.75),
            CornerRadius::same(4),
            Stroke::new(1.5, pal.text_muted),
            StrokeKind::Inside,
        );
    }
}

/// Round badge with mark (corner of a selected map cell).
pub(crate) fn paint_check_badge(painter: &egui::Painter, env: &mut Env, center: egui::Pos2) {
    let pal = env.pal;
    painter.circle_filled(center, 10.0, pal.accent);
    env.icons.paint(
        painter,
        Rect::from_center_size(center, vec2(13.0, 13.0)),
        Icon::Checkmark,
        Color32::WHITE,
    );
}

/// Accent veil of a selected map cell: distinguished from the outline of the active selection (3 px outside),
/// of the focus and the cart logo.
pub(crate) fn mark_tint(pal: &theme::Palette) -> Color32 {
    pal.accent.gamma_multiply(0.30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternar_marca_y_desmarca_y_fija_el_ancla() {
        let mut m = Marks::default();
        m.toggle(7);
        m.toggle(3);
        assert_eq!(m.sorted(), vec![3, 7]);
        assert_eq!(m.anchor, Some(3));
        m.toggle(7);
        assert_eq!(m.sorted(), vec![3]);
        m.clear();
        assert!(m.is_empty() && m.anchor.is_none());
    }

    #[test]
    fn invertir_marca_lo_no_marcado_y_viceversa() {
        let mut m = Marks::default();
        let all: Vec<NodeId> = (1..=5).collect();
        m.add(all.iter().copied());
        m.toggle(4);
        assert_eq!(m.sorted(), vec![1, 2, 3, 5]);
        m.invert(&all);
        assert_eq!(m.sorted(), vec![4]);
    }

    #[test]
    fn el_rango_incluye_los_dos_extremos_en_cualquier_sentido() {
        let order: Vec<NodeId> = vec![10, 20, 30, 40, 50];
        assert_eq!(range_between(&order, 20, 40), &[20, 30, 40]);
        assert_eq!(range_between(&order, 40, 20), &[20, 30, 40]);
        assert_eq!(range_between(&order, 30, 30), &[30]);
        assert!(range_between(&order, 20, 99).is_empty());
    }

    #[test]
    fn el_resumen_dice_que_no_se_puede_añadir_y_por_que() {
        let tr = Tr::new(crate::Lang::Es);
        let s = Summary {
            marked: 4,
            add: vec![1, 2],
            contents_of: HashSet::from([2]),
            bytes: 3 << 30,
            lower: false,
            in_basket: 1,
            protected: 1,
            info: 0,
            cloud: 0,
        };
        let (a, b) = summary_lines(&tr, &s);
        assert_eq!(a, "4 seleccionados · se añadirían 3,0 GB");
        assert_eq!(
            b.as_deref(),
            Some(
                "2 no se pueden añadir: 1 protegido, 1 ya en la cesta · 1 carpeta conocida: se añade su contenido"
            )
        );
        // Nothing to add: no size.
        let none = Summary {
            marked: 1,
            protected: 1,
            ..Summary::default()
        };
        let (a, b) = summary_lines(&tr, &none);
        assert_eq!(a, "1 seleccionado");
        assert_eq!(b.as_deref(), Some("1 no se puede añadir: 1 protegido"));
        let en = Tr::new(crate::Lang::En);
        let (a, b) = summary_lines(&en, &s);
        assert_eq!(a, "4 selected · 3.0 GB would be added");
        assert!(
            b.unwrap()
                .starts_with("2 cannot be added: 1 protected, 1 already in the basket")
        );
        // Without anything excluded there is no second line.
        let ok = Summary {
            marked: 2,
            add: vec![1, 2],
            bytes: 1 << 20,
            ..Summary::default()
        };
        assert!(summary_lines(&tr, &ok).1.is_none());
    }

    fn key_events(keys: &[(egui::Key, bool)]) -> egui::InputState {
        let mut i = egui::InputState::default();
        for &(key, repeat) in keys {
            i.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat,
                modifiers: egui::Modifiers::NONE,
            });
        }
        i
    }

    #[test]
    fn un_clic_sin_puntero_alterna_salvo_con_intro_o_la_autorrepeticion_de_espacio() {
        use egui::Key::{Enter, Space};
        // Keyless: The default click of a screen reader.
        assert!(keyboard_click_toggles(&key_events(&[])));
        // The first press of Space yes; self-repetition alone, no.
        assert!(keyboard_click_toggles(&key_events(&[(Space, false)])));
        assert!(keyboard_click_toggles(&key_events(&[
            (Space, false),
            (Space, true)
        ])));
        assert!(!keyboard_click_toggles(&key_events(&[(Space, true)])));
        // Intro never alternates.
        assert!(!keyboard_click_toggles(&key_events(&[(Enter, false)])));
        assert!(space_pressed_once(&key_events(&[(Space, false)])));
        assert!(!space_pressed_once(&key_events(&[(Space, true)])));
        assert!(!space_pressed_once(&key_events(&[(Enter, false)])));
    }

    #[test]
    fn una_rafaga_de_mayus_flechas_amplia_y_encoge_sobre_lo_marcado_antes() {
        let order: Vec<NodeId> = (1..=8).collect();
        let mut m = Marks::default();
        m.toggle(7); // It was already selected before the burst.
        m.anchor = Some(2);
        let run = |m: &mut Marks, to: NodeId| {
            let r = range_between(&order, 2, to).to_vec();
            m.set_run(&r);
        };
        run(&mut m, 3);
        assert_eq!(m.sorted(), vec![2, 3, 7]);
        run(&mut m, 5);
        assert_eq!(m.sorted(), vec![2, 3, 4, 5, 7]);
        run(&mut m, 4);
        assert_eq!(
            m.sorted(),
            vec![2, 3, 4, 7],
            "encoge y conserva lo de antes"
        );
        // Any other action ends the burst: the next part of what there is.
        m.toggle(8);
        run(&mut m, 3);
        assert_eq!(m.sorted(), vec![2, 3, 4, 7, 8]);
    }
}
