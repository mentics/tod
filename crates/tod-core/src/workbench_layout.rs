//! The workbench's column widths, kept across restarts in
//! `workbench-layout.json` beside the task list's working set.
//!
//! Panels are not reopened on launch, so a width belongs to a column's
//! position (the second column, the third…), not to the panel in it: the
//! next panel opened there takes it. A width the user never dragged is
//! `None`, and that column shares the space left over.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "workbench-layout.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkbenchLayout {
    /// The node tree's width in pixels, once the user has dragged it.
    #[serde(default)]
    pub tree_width: Option<f32>,
    /// Columns 2 onward, by position.
    #[serde(default)]
    pub column_widths: Vec<Option<f32>>,
}

impl WorkbenchLayout {
    /// The saved width for the column at `position` (0 is the one right of
    /// the tree), if the user dragged one there.
    pub fn column_width(&self, position: usize) -> Option<f32> {
        self.column_widths.get(position).copied().flatten()
    }

    /// Record the width of the column at `position`, keeping the widths of
    /// positions not open now.
    pub fn set_column_width(&mut self, position: usize, width: Option<f32>) {
        if self.column_widths.len() <= position {
            self.column_widths.resize(position + 1, None);
        }
        self.column_widths[position] = width;
    }
}

/// The saved layout, or the default when there is none or it cannot be read.
pub fn load(config_dir: &Path) -> WorkbenchLayout {
    fs::read_to_string(config_dir.join(FILE_NAME))
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_default()
}

pub fn save(config_dir: &Path, layout: &WorkbenchLayout) -> std::io::Result<()> {
    let body = serde_json::to_string_pretty(layout).map_err(std::io::Error::other)?;
    fs::write(config_dir.join(FILE_NAME), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tod-workbench-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_saved_layout_loads_back() {
        let dir = scratch_dir();
        let mut layout = WorkbenchLayout {
            tree_width: Some(412.),
            ..Default::default()
        };
        layout.set_column_width(2, Some(300.));
        save(&dir, &layout).unwrap();

        let loaded = load(&dir);
        assert_eq!(loaded, layout);
        assert_eq!(loaded.column_width(0), None);
        assert_eq!(loaded.column_width(2), Some(300.));
        assert_eq!(loaded.column_width(7), None);
    }

    #[test]
    fn a_missing_or_unreadable_file_is_the_default() {
        let dir = scratch_dir();
        assert_eq!(load(&dir), WorkbenchLayout::default());
        fs::write(dir.join(FILE_NAME), "not json").unwrap();
        assert_eq!(load(&dir), WorkbenchLayout::default());
    }
}
