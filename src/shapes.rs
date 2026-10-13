//! Layouts without their panes: the shape of a window's splits, to save
//! and to give a running window, like tmux's `select-layout`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::layout::{Axis, Layout, PaneId};

/// The splits that build a layout from its first pane: each splits the
/// pane at that index (0 the first, then one more per split, in order)
/// along an axis, keeping a share of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub splits: Vec<(usize, Axis, f32)>,
}

/// The layouts imkr has without saving, as tmux names them.
pub const PRESETS: &[(&str, &str)] = &[
    ("even-horizontal", "all side by side"),
    ("even-vertical", "all stacked"),
    (
        "main-vertical",
        "one large on the left, the rest stacked on the right",
    ),
    (
        "main-horizontal",
        "one large on top, the rest side by side below",
    ),
    ("tiled", "a grid"),
];

impl Shape {
    /// How many panes it has places for.
    pub fn panes(&self) -> usize {
        self.splits.len() + 1
    }

    /// The shape of `layout`.
    pub fn of(layout: &Layout) -> Option<Self> {
        let (first, splits) = layout.splits()?;
        let mut order = vec![first];
        let mut out = Vec::new();
        for split in splits {
            let of = order.iter().position(|&p| p == split.of)?;
            out.push((of, split.axis, split.ratio));
            order.push(split.new);
        }
        Some(Shape { splits: out })
    }

    /// The preset `name` for `n` panes.
    pub fn preset(name: &str, n: usize) -> Option<Self> {
        let n = n.max(1);
        let chain = |axis| (0..n - 1).map(|i| (i, axis, 0.5)).collect();
        let splits = match name {
            "even-horizontal" => chain(Axis::Row),
            "even-vertical" => chain(Axis::Column),
            // The main pane, then the rest stacked from the second.
            "main-vertical" | "main-horizontal" => {
                let (main, rest) = if name == "main-vertical" {
                    (Axis::Row, Axis::Column)
                } else {
                    (Axis::Column, Axis::Row)
                };
                let mut splits = Vec::new();
                if n > 1 {
                    splits.push((0, main, 0.6));
                }
                // The rest in equal shares: each split keeps 1/(left) of it.
                let rest_panes = n.saturating_sub(1);
                for i in 1..rest_panes {
                    splits.push((i, rest, 1.0 / (rest_panes + 1 - i) as f32));
                }
                splits
            }
            // Rows of about √n panes each, the last with the rest.
            "tiled" => {
                let columns = (n as f32).sqrt().ceil() as usize;
                let rows = n.div_ceil(columns);
                // The first pane of each row, stacked: panes 0 … rows-1.
                let mut splits: Vec<(usize, Axis, f32)> =
                    (1..rows).map(|row| (row - 1, Axis::Column, 0.5)).collect();
                let mut next = rows;
                for row in 0..rows {
                    let in_row = if row + 1 == rows {
                        n - columns * (rows - 1)
                    } else {
                        columns
                    };
                    let mut last = row;
                    for _ in 1..in_row {
                        splits.push((last, Axis::Row, 0.5));
                        last = next;
                        next += 1;
                    }
                }
                splits
            }
            _ => return None,
        };
        Some(Shape { splits })
    }

    /// A layout of this shape with `panes` in its places, in order; more
    /// panes than places are added after the last, side by side. Even
    /// shares when `even`.
    pub fn build(&self, panes: &[PaneId], even: bool) -> Option<Layout> {
        let mut layout = Layout::new(*panes.first()?);
        let mut placed = 1;
        for &(of, axis, ratio) in &self.splits {
            let (Some(&of), Some(&new)) = (panes.get(of), panes.get(placed)) else {
                break;
            };
            layout.split_at(of, axis, new, ratio);
            placed += 1;
        }
        for &new in &panes[placed..] {
            layout.split(panes[placed - 1], Axis::Row, new);
            placed += 1;
        }
        if even {
            layout.equalize();
        }
        Some(layout)
    }
}

/// Where saved layouts are kept: `layouts.json` next to the config.
fn path() -> Option<PathBuf> {
    Some(crate::config::path().ok()?.parent()?.join("layouts.json"))
}

/// The layouts saved by name.
pub fn saved() -> BTreeMap<String, Shape> {
    path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Saves `shape` as `name`, replacing one of that name.
pub fn save(name: &str, shape: Shape) -> Result<()> {
    let path = path().context("no config directory")?;
    let mut all = saved();
    all.insert(name.to_owned(), shape);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&all)? + "\n")?;
    Ok(())
}

/// Removes the saved layout `name`; false if there is none.
pub fn remove(name: &str) -> Result<bool> {
    let path = path().context("no config directory")?;
    let mut all = saved();
    if all.remove(name).is_none() {
        return Ok(false);
    }
    std::fs::write(&path, serde_json::to_string_pretty(&all)? + "\n")?;
    Ok(true)
}

/// The shape `name` stands for with `n` panes: a saved one, else a
/// preset.
pub fn named(name: &str, n: usize) -> Option<(Shape, bool)> {
    match saved().remove(name) {
        Some(shape) => Some((shape, false)),
        // The main pane keeps its larger share.
        None => Shape::preset(name, n).map(|shape| (shape, !name.starts_with("main-"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn places(layout: &Layout) -> Vec<(PaneId, Rect)> {
        layout.rects(Rect::new(0, 0, 120, 40))
    }

    #[test]
    fn presets_place_every_pane() {
        for (name, _) in PRESETS {
            for n in 1..=7 {
                let shape = Shape::preset(name, n).unwrap();
                assert_eq!(shape.panes(), n, "{name} for {n}");
                let panes: Vec<PaneId> = (10..10 + n as PaneId).collect();
                let layout = shape.build(&panes, true).unwrap();
                let mut got = layout.panes();
                got.sort_unstable();
                assert_eq!(got, panes, "{name} for {n}");
            }
        }
        let side = places(
            &Shape::preset("even-horizontal", 3)
                .unwrap()
                .build(&[1, 2, 3], true)
                .unwrap(),
        );
        assert!(side.iter().all(|(_, r)| r.height == 40 && r.width == 40));
        let main = places(
            &Shape::preset("main-vertical", 4)
                .unwrap()
                .build(&[1, 2, 3, 4], false)
                .unwrap(),
        );
        assert_eq!(main[0].1.width, 72);
        assert!(
            main[1..]
                .iter()
                .all(|(_, r)| r.height >= 13 && r.height <= 14)
        );
        let grid = places(
            &Shape::preset("tiled", 4)
                .unwrap()
                .build(&[1, 2, 3, 4], true)
                .unwrap(),
        );
        assert!(grid.iter().all(|(_, r)| r.width == 60 && r.height == 20));
    }

    #[test]
    fn a_shape_round_trips_and_takes_other_panes() {
        let mut layout = Layout::new(1);
        layout.split_at(1, Axis::Row, 2, 0.7);
        layout.split(2, Axis::Column, 3);
        let shape = Shape::of(&layout).unwrap();
        assert_eq!(shape.panes(), 3);
        let again = shape.build(&[7, 8, 9], false).unwrap();
        let rects = |l: &Layout| places(l).into_iter().map(|(_, r)| r).collect::<Vec<_>>();
        assert_eq!(rects(&again), rects(&layout));
        // More panes than places: all of them still there.
        assert_eq!(shape.build(&[1, 2, 3, 4], false).unwrap().panes().len(), 4);
    }
}
