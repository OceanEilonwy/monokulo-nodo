//! Where panels sit on the screen's grid.
//!
//! Panels never overlap, never leave the grid and keep within each panel's size bounds. Every
//! change goes through a method that checks this, and a layout read from disk is repaired on load,
//! so the drawing code can trust it.

use std::collections::BTreeMap;
use std::path::Path;

use nodo_manifest::PANEL_GRID;
use serde::{Deserialize, Serialize};

pub const COLUMNS: u8 = PANEL_GRID[0];
pub const ROWS: u8 = PANEL_GRID[1];
const LAYOUT_VERSION: u32 = 1;

/// A panel, named by its package and its id within the package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PanelKey {
    pub package: String,
    pub panel: String,
}

impl PanelKey {
    pub fn new(package: &str, panel: &str) -> Self {
        PanelKey { package: package.to_string(), panel: panel.to_string() }
    }
}

/// What the layout needs to know about a panel that can be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeRule {
    pub min: [u8; 2],
    pub max: [u8; 2],
    pub initial: [u8; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    #[serde(flatten)]
    pub key: PanelKey,
    pub x: u8,
    pub y: u8,
    pub w: u8,
    pub h: u8,
}

impl Placement {
    fn overlaps(&self, other: &Placement) -> bool {
        self.x < other.x + other.w && other.x < self.x + self.w && self.y < other.y + other.h && other.y < self.y + self.h
    }

    fn inside_grid(&self) -> bool {
        self.w >= 1 && self.h >= 1 && self.x + self.w <= COLUMNS && self.y + self.h <= ROWS
    }

    fn within(&self, rule: &SizeRule) -> bool {
        (rule.min[0]..=rule.max[0]).contains(&self.w) && (rule.min[1]..=rule.max[1]).contains(&self.h)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LayoutFile {
    version: u32,
    panels: Vec<Placement>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub placements: Vec<Placement>,
}

impl Layout {
    /// Reads the layout, keeping only placements that name a panel in `rules` and fit together.
    /// Returns what was dropped, for the log. With no file, every panel is placed in order.
    pub fn load(path: &Path, rules: &BTreeMap<PanelKey, SizeRule>) -> (Layout, Vec<String>) {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<LayoutFile>(&bytes) {
                Ok(file) if file.version == LAYOUT_VERSION => Layout::repaired(file.panels, rules),
                _ => {
                    let mut layout = Layout::default();
                    layout.place_all(rules);
                    (layout, vec!["the layout file could not be read, so panels were placed again".into()])
                }
            },
            Err(_) => {
                let mut layout = Layout::default();
                layout.place_all(rules);
                (layout, Vec::new())
            }
        }
    }

    fn repaired(placements: Vec<Placement>, rules: &BTreeMap<PanelKey, SizeRule>) -> (Layout, Vec<String>) {
        let mut layout = Layout::default();
        let mut dropped = Vec::new();
        for placement in placements {
            let name = format!("{}/{}", placement.key.package, placement.key.panel);
            let Some(rule) = rules.get(&placement.key) else {
                dropped.push(format!("{name} is no longer offered"));
                continue;
            };
            if !placement.inside_grid() || !placement.within(rule) {
                dropped.push(format!("{name} did not fit its size or the grid"));
            } else if layout.placements.iter().any(|p| p.key == placement.key || p.overlaps(&placement)) {
                dropped.push(format!("{name} overlapped another panel"));
            } else {
                layout.placements.push(placement);
            }
        }
        (layout, dropped)
    }

    /// Writes the layout atomically: a crash leaves the old file or the new one, never half.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let file = LayoutFile { version: LAYOUT_VERSION, panels: self.placements.clone() };
        let text = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, &text)?;
        // On disk before the rename, so that a power cut cannot leave an empty layout behind.
        file.sync_all()?;
        drop(file);
        std::fs::rename(tmp, path)
    }

    /// Places every panel in `rules` that is not placed yet, wherever it first fits.
    pub fn place_all(&mut self, rules: &BTreeMap<PanelKey, SizeRule>) {
        for key in rules.keys() {
            self.add(key, rules);
        }
    }

    pub fn is_placed(&self, key: &PanelKey) -> bool {
        self.placements.iter().any(|p| &p.key == key)
    }

    /// Panels that can be shown but are not, in order.
    pub fn hidden<'a>(&self, rules: &'a BTreeMap<PanelKey, SizeRule>) -> Vec<&'a PanelKey> {
        rules.keys().filter(|key| !self.is_placed(key)).collect()
    }

    /// Adds a panel at its initial size where it first fits, else at its smallest size. False when
    /// it is already placed or nothing fits.
    pub fn add(&mut self, key: &PanelKey, rules: &BTreeMap<PanelKey, SizeRule>) -> bool {
        let Some(rule) = rules.get(key) else { return false };
        if self.is_placed(key) {
            return false;
        }
        for [w, h] in [rule.initial, rule.min] {
            for y in 0..ROWS {
                for x in 0..COLUMNS {
                    let candidate = Placement { key: key.clone(), x, y, w, h };
                    if candidate.inside_grid() && !self.placements.iter().any(|p| p.overlaps(&candidate)) {
                        self.placements.push(candidate);
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn hide(&mut self, index: usize) {
        if index < self.placements.len() {
            self.placements.remove(index);
        }
    }

    /// Moves a panel by whole cells. False, and nothing changes, if it would leave the grid or
    /// overlap another panel.
    pub fn try_move(&mut self, index: usize, dx: i8, dy: i8) -> bool {
        self.try_change(index, None, |p| {
            p.x = p.x.checked_add_signed(dx)?;
            p.y = p.y.checked_add_signed(dy)?;
            Some(())
        })
    }

    /// Moves a panel to a cell, as a drag does.
    pub fn try_move_to(&mut self, index: usize, x: u8, y: u8) -> bool {
        self.try_change(index, None, |p| {
            p.x = x;
            p.y = y;
            Some(())
        })
    }

    /// Grows or shrinks a panel from its bottom-right corner, within its size bounds.
    pub fn try_resize(&mut self, index: usize, dw: i8, dh: i8, rule: &SizeRule) -> bool {
        self.try_change(index, Some(rule), |p| {
            p.w = p.w.checked_add_signed(dw)?;
            p.h = p.h.checked_add_signed(dh)?;
            Some(())
        })
    }

    /// Sets a panel's size, as dragging its corner does.
    pub fn try_resize_to(&mut self, index: usize, w: u8, h: u8, rule: &SizeRule) -> bool {
        self.try_change(index, Some(rule), |p| {
            p.w = w;
            p.h = h;
            Some(())
        })
    }

    fn try_change(&mut self, index: usize, rule: Option<&SizeRule>, change: impl FnOnce(&mut Placement) -> Option<()>) -> bool {
        let Some(current) = self.placements.get(index) else { return false };
        let mut candidate = current.clone();
        if change(&mut candidate).is_none() || !candidate.inside_grid() {
            return false;
        }
        if rule.is_some_and(|rule| !candidate.within(rule)) {
            return false;
        }
        let clashes = self.placements.iter().enumerate().any(|(i, p)| i != index && p.overlaps(&candidate));
        if clashes {
            return false;
        }
        self.placements[index] = candidate;
        true
    }

    /// Whether panel `index`, keeping its size, could sit with its top-left cell at (`x`, `y`).
    pub fn fits_at(&self, index: usize, x: u8, y: u8) -> bool {
        let Some(current) = self.placements.get(index) else { return false };
        let candidate = Placement { x, y, ..current.clone() };
        candidate.inside_grid() && !self.placements.iter().enumerate().any(|(i, p)| i != index && p.overlaps(&candidate))
    }

    /// Where to put panel `index` so that it covers cell (`x`, `y`): that cell as its top-left
    /// if it fits there, else the nearest top-left that still covers the cell. None when the panel
    /// cannot cover that cell at all.
    pub fn anchor_covering(&self, index: usize, x: u8, y: u8) -> Option<(u8, u8)> {
        let p = self.placements.get(index)?;
        let mut options = Vec::new();
        for dy in 0..p.h {
            for dx in 0..p.w {
                if let (Some(ax), Some(ay)) = (x.checked_sub(dx), y.checked_sub(dy)) {
                    options.push((u16::from(dx) + u16::from(dy), ax, ay));
                }
            }
        }
        options.sort();
        options.into_iter().find(|(_, ax, ay)| self.fits_at(index, *ax, *ay)).map(|(_, ax, ay)| (ax, ay))
    }

    /// Every size `rule` allows, smallest area first, then narrowest.
    pub fn sizes(rule: &SizeRule) -> Vec<(u8, u8)> {
        let mut sizes: Vec<(u8, u8)> = (rule.min[1]..=rule.max[1])
            .flat_map(|h| (rule.min[0]..=rule.max[0]).map(move |w| (w, h)))
            .filter(|(w, h)| *w <= COLUMNS && *h <= ROWS)
            .collect();
        sizes.sort_by_key(|(w, h)| (u16::from(*w) * u16::from(*h), *w));
        sizes
    }

    /// The size panel `index` takes when its Size button is tapped: the next allowed size after
    /// its current one, wrapping round, that fits where it is. It grows from its top-left cell,
    /// and moves left or up only when the grid's edge is in the way.
    pub fn next_size(&self, index: usize, rule: &SizeRule) -> Option<Placement> {
        let current = self.placements.get(index)?;
        let sizes = Self::sizes(rule);
        let at = sizes.iter().position(|s| *s == (current.w, current.h)).unwrap_or(0);
        (1..sizes.len()).map(|i| sizes[(at + i) % sizes.len()]).find_map(|(w, h)| {
            let x = current.x.min(COLUMNS - w);
            let y = current.y.min(ROWS - h);
            let candidate = Placement { key: current.key.clone(), x, y, w, h };
            let clashes = self.placements.iter().enumerate().any(|(i, p)| i != index && p.overlaps(&candidate));
            (!clashes).then_some(candidate)
        })
    }

    /// Applies [`Layout::next_size`]. False when no other size fits.
    pub fn cycle_size(&mut self, index: usize, rule: &SizeRule) -> bool {
        match self.next_size(index, rule) {
            Some(next) => {
                self.placements[index] = next;
                true
            }
            None => false,
        }
    }

    /// Adds a panel so that it covers cell (`x`, `y`), at its initial size if that fits there, else
    /// its smallest. Falls back to [`Layout::add`]'s first free space. False when nothing fits.
    pub fn add_covering(&mut self, key: &PanelKey, rules: &BTreeMap<PanelKey, SizeRule>, x: u8, y: u8) -> bool {
        let Some(rule) = rules.get(key) else { return false };
        if self.is_placed(key) {
            return false;
        }
        for [w, h] in [rule.initial, rule.min] {
            let mut options = Vec::new();
            for dy in 0..h {
                for dx in 0..w {
                    if let (Some(ax), Some(ay)) = (x.checked_sub(dx), y.checked_sub(dy)) {
                        options.push((u16::from(dx) + u16::from(dy), ax, ay));
                    }
                }
            }
            options.sort();
            for (_, ax, ay) in options {
                let candidate = Placement { key: key.clone(), x: ax, y: ay, w, h };
                if candidate.inside_grid() && !self.placements.iter().any(|p| p.overlaps(&candidate)) {
                    self.placements.push(candidate);
                    return true;
                }
            }
        }
        self.add(key, rules)
    }

    /// Whether `candidate` could replace placement `index`: inside the grid, within `rule` when
    /// given, and clear of every other panel. Dragging shows where a tile would land with this.
    pub fn allows(&self, index: usize, candidate: &Placement, rule: Option<&SizeRule>) -> bool {
        candidate.inside_grid()
            && rule.is_none_or(|rule| candidate.within(rule))
            && !self.placements.iter().enumerate().any(|(i, p)| i != index && p.overlaps(candidate))
    }

    /// Replaces placement `index` with `candidate` if [`Layout::allows`] it.
    pub fn try_set(&mut self, index: usize, candidate: Placement, rule: Option<&SizeRule>) -> bool {
        if index < self.placements.len() && self.placements[index].key == candidate.key && self.allows(index, &candidate, rule) {
            self.placements[index] = candidate;
            true
        } else {
            false
        }
    }

    /// Puts back a placement that was hidden, at its old position in the list, if it still fits.
    pub fn restore(&mut self, index: usize, placement: Placement) -> bool {
        if !placement.inside_grid() || self.is_placed(&placement.key) || self.placements.iter().any(|p| p.overlaps(&placement)) {
            return false;
        }
        self.placements.insert(index.min(self.placements.len()), placement);
        true
    }

    /// The panel covering a cell, if any.
    pub fn at(&self, x: u8, y: u8) -> Option<usize> {
        self.placements.iter().position(|p| x >= p.x && x < p.x + p.w && y >= p.y && y < p.y + p.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (package, min, max, initial)
    type Rule<'a> = (&'a str, [u8; 2], [u8; 2], [u8; 2]);

    fn rules(list: &[Rule]) -> BTreeMap<PanelKey, SizeRule> {
        list.iter().map(|(name, min, max, initial)| (PanelKey::new(name, "p"), SizeRule { min: *min, max: *max, initial: *initial })).collect()
    }

    fn no_overlaps(layout: &Layout) -> bool {
        let p = &layout.placements;
        (0..p.len()).all(|i| p[i].inside_grid() && (i + 1..p.len()).all(|j| !p[i].overlaps(&p[j])))
    }

    #[test]
    fn panels_are_placed_in_order_where_they_fit() {
        let r = rules(&[("a", [2, 1], [4, 2], [4, 2]), ("b", [2, 1], [4, 2], [4, 2]), ("c", [2, 2], [4, 4], [8, 4])]);
        let mut layout = Layout::default();
        layout.place_all(&r);
        let a = &layout.placements[0];
        let b = &layout.placements[1];
        assert_eq!((a.x, a.y, a.w, a.h), (0, 0, 4, 2));
        assert_eq!((b.x, b.y, b.w, b.h), (4, 0, 4, 2));
        // c's initial 8x4 cannot fit, so it takes its smallest size in the first free space.
        let c = &layout.placements[2];
        assert_eq!((c.x, c.y, c.w, c.h), (0, 2, 2, 2));
        assert!(no_overlaps(&layout));
        assert!(layout.hidden(&r).is_empty());
    }

    #[test]
    fn moves_and_resizes_that_would_clash_are_refused() {
        let r = rules(&[("a", [1, 1], [4, 2], [2, 1]), ("b", [1, 1], [4, 2], [2, 1])]);
        let mut layout = Layout::default();
        layout.place_all(&r);
        let rule = r[&PanelKey::new("a", "p")];
        assert!(!layout.try_move(0, 1, 0), "into b");
        assert!(!layout.try_move(0, -1, 0), "off the grid");
        assert!(layout.try_move(0, 0, 1));
        assert!(layout.try_resize(0, 2, 1, &rule));
        assert!(!layout.try_resize(0, 1, 0, &rule), "beyond max_size");
        assert!(!layout.try_resize(0, -4, 0, &rule), "below min_size");
        assert!(!layout.try_move_to(0, 7, 0), "would leave the grid");
        assert!(layout.try_move_to(0, 4, 2));
        assert_eq!(layout.at(5, 3), Some(0));
        assert_eq!(layout.at(0, 3), None);
        assert!(no_overlaps(&layout));
    }

    #[test]
    fn tapping_a_free_cell_finds_a_place_and_size_cycles() {
        let r = rules(&[("a", [2, 1], [4, 2], [2, 2]), ("b", [2, 1], [4, 2], [2, 1])]);
        let mut layout = Layout::default();
        layout.place_all(&r);
        // a at (0,0) 2x2, b at (2,0) 2x1.
        assert!(layout.fits_at(0, 6, 2));
        assert!(!layout.fits_at(0, 7, 0), "would leave the grid");
        assert!(!layout.fits_at(0, 1, 0), "would overlap b");
        assert_eq!(layout.anchor_covering(0, 7, 3), Some((6, 2)), "the tapped cell becomes its bottom-right");
        assert_eq!(layout.anchor_covering(0, 4, 0), Some((4, 0)));
        let rule = r[&PanelKey::new("a", "p")];
        assert_eq!(Layout::sizes(&rule).first(), Some(&(2, 1)));
        assert_eq!(Layout::sizes(&rule).last(), Some(&(4, 2)));
        // After 2x2 come 4x1, 3x2 and 4x2, which all hit b, so it wraps round to 2x1, then 3x1
        // would hit b too and 2x2 comes back.
        assert!(layout.cycle_size(0, &rule));
        assert_eq!((layout.placements[0].w, layout.placements[0].h), (2, 1));
        assert!(layout.cycle_size(0, &rule));
        assert_eq!((layout.placements[0].w, layout.placements[0].h), (2, 2));
        let hidden = layout.placements[1].clone();
        layout.hide(1);
        assert!(layout.restore(1, hidden.clone()));
        assert!(!layout.restore(1, hidden), "already back");
        assert!(no_overlaps(&layout));
    }

    #[test]
    fn adding_where_tapped_and_checking_a_drag() {
        let r = rules(&[("a", [2, 1], [4, 2], [4, 2]), ("b", [2, 1], [4, 2], [2, 1])]);
        let mut layout = Layout::default();
        let a = PanelKey::new("a", "p");
        assert!(layout.add_covering(&a, &r, 7, 3));
        let p = &layout.placements[0];
        assert_eq!((p.x, p.y, p.w, p.h), (4, 2, 4, 2), "covers the tapped bottom-right cell");
        assert!(layout.add_covering(&PanelKey::new("b", "p"), &r, 5, 3), "falls back to the first free space");
        assert_eq!((layout.placements[1].x, layout.placements[1].y), (0, 0));
        let rule = r[&a];
        let moved = Placement { x: 0, y: 2, ..layout.placements[0].clone() };
        assert!(layout.allows(0, &moved, Some(&rule)));
        let clash = Placement { x: 0, y: 0, ..layout.placements[0].clone() };
        assert!(!layout.allows(0, &clash, Some(&rule)), "b is there");
        let too_big = Placement { w: 5, ..moved.clone() };
        assert!(!layout.allows(0, &too_big, Some(&rule)), "beyond max_size");
        assert!(layout.try_set(0, moved, Some(&rule)));
        assert!(no_overlaps(&layout));
    }

    #[test]
    fn hide_and_add_back() {
        let r = rules(&[("a", [2, 1], [4, 2], [4, 2])]);
        let mut layout = Layout::default();
        layout.place_all(&r);
        layout.hide(0);
        assert_eq!(layout.hidden(&r), vec![&PanelKey::new("a", "p")]);
        assert!(layout.add(&PanelKey::new("a", "p"), &r));
        assert!(!layout.add(&PanelKey::new("a", "p"), &r), "already placed");
        assert!(!layout.add(&PanelKey::new("gone", "p"), &r), "not offered");
    }

    #[test]
    fn a_layout_file_is_repaired_on_load() {
        let dir = std::env::temp_dir().join(format!("nodo-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");
        std::fs::write(
            &path,
            r#"{"version":1,"panels":[
              {"package":"a","panel":"p","x":0,"y":0,"w":4,"h":2},
              {"package":"b","panel":"p","x":2,"y":0,"w":2,"h":1},
              {"package":"gone","panel":"p","x":6,"y":0,"w":2,"h":1},
              {"package":"c","panel":"p","x":7,"y":3,"w":2,"h":1}]}"#,
        )
        .unwrap();
        let r = rules(&[("a", [2, 1], [4, 2], [4, 2]), ("b", [2, 1], [4, 2], [2, 1]), ("c", [2, 1], [4, 2], [2, 1])]);
        let (layout, dropped) = Layout::load(&path, &r);
        assert_eq!(layout.placements.len(), 1, "only a survives");
        assert_eq!(dropped.len(), 3, "{dropped:?}");
        layout.save(&path).unwrap();
        assert_eq!(Layout::load(&path, &r).0, layout);
        std::fs::write(&path, "not json").unwrap();
        let (fresh, note) = Layout::load(&path, &r);
        assert_eq!(fresh.placements.len(), 3);
        assert_eq!(note.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
