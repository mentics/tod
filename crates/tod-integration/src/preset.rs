//! Filter preset storage for Linear data source.
//!
//! Presets store filter field values only (not result cap) and are shared across
//! all Linear generators. They are stored as JSON files in the data root.

use serde::{Deserialize, Serialize};
use std::path::Path;

const PRESETS_FILE: &str = "linear_filter_presets.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterPreset {
    pub name: String,
    pub filters: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PresetsFile {
    presets: Vec<FilterPreset>,
}

/// Load all presets from the data root, sorted alphabetically by name (case-insensitive).
pub fn load_presets(data_root: &Path) -> Result<Vec<FilterPreset>, String> {
    let path = data_root.join(PRESETS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read presets file: {}", e))?;
    let file: PresetsFile = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse presets file: {}", e))?;

    let mut presets = file.presets;
    presets.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(presets)
}

/// Save a preset. If a preset with the same name (case-insensitive) exists, it is overwritten.
pub fn save_preset(
    data_root: &Path,
    name: &str,
    filters: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let mut presets = load_presets(data_root)?;

    // Remove existing preset with same name (case-insensitive)
    presets.retain(|p| p.name.to_lowercase() != name.to_lowercase());

    // Add new preset
    presets.push(FilterPreset {
        name: name.to_string(),
        filters: filters.clone(),
    });

    write_presets(data_root, &presets)
}

/// Delete a preset by name (case-insensitive).
pub fn delete_preset(data_root: &Path, name: &str) -> Result<bool, String> {
    let mut presets = load_presets(data_root)?;
    let before = presets.len();
    presets.retain(|p| p.name.to_lowercase() != name.to_lowercase());
    let deleted = presets.len() < before;

    if deleted {
        write_presets(data_root, &presets)?;
    }

    Ok(deleted)
}

/// Rename a preset (case-insensitive match on old name).
pub fn rename_preset(data_root: &Path, old_name: &str, new_name: &str) -> Result<bool, String> {
    let mut presets = load_presets(data_root)?;

    // Check if new name already exists (and it's not the same preset)
    if presets.iter().any(|p|
        p.name.to_lowercase() == new_name.to_lowercase()
        && p.name.to_lowercase() != old_name.to_lowercase()
    ) {
        return Err(format!("A preset named '{}' already exists", new_name));
    }

    let found = presets.iter_mut().find(|p| p.name.to_lowercase() == old_name.to_lowercase());
    if let Some(preset) = found {
        preset.name = new_name.to_string();
        write_presets(data_root, &presets)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn write_presets(data_root: &Path, presets: &[FilterPreset]) -> Result<(), String> {
    let file = PresetsFile {
        presets: presets.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("Failed to serialize presets: {}", e))?;
    let path = data_root.join(PRESETS_FILE);
    std::fs::write(&path, json)
        .map_err(|e| format!("Failed to write presets file: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        use std::time::SystemTime;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tod-preset-test-{}", nanos));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_presets_returns_empty_when_file_absent() {
        let dir = temp_dir();
        let presets = load_presets(&dir).unwrap();
        assert_eq!(presets.len(), 0);
    }

    #[test]
    fn save_and_load_preset() {
        let dir = temp_dir();
        let mut filters = serde_json::Map::new();
        filters.insert("state".into(), serde_json::json!({"eq": "In Progress"}));

        save_preset(&dir, "My Preset", &filters).unwrap();
        let presets = load_presets(&dir).unwrap();

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "My Preset");
        assert_eq!(presets[0].filters.get("state"), Some(&serde_json::json!({"eq": "In Progress"})));
    }

    #[test]
    fn save_overwrites_existing_preset_case_insensitive() {
        let dir = temp_dir();
        let mut filters1 = serde_json::Map::new();
        filters1.insert("state".into(), serde_json::json!({"eq": "In Progress"}));
        save_preset(&dir, "My Preset", &filters1).unwrap();

        let mut filters2 = serde_json::Map::new();
        filters2.insert("priority".into(), serde_json::json!({"eq": "High"}));
        save_preset(&dir, "my preset", &filters2).unwrap();

        let presets = load_presets(&dir).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "my preset");
        assert_eq!(presets[0].filters.get("priority"), Some(&serde_json::json!({"eq": "High"})));
        assert_eq!(presets[0].filters.get("state"), None);
    }

    #[test]
    fn presets_sorted_alphabetically_case_insensitive() {
        let dir = temp_dir();
        save_preset(&dir, "Zebra", &serde_json::Map::new()).unwrap();
        save_preset(&dir, "apple", &serde_json::Map::new()).unwrap();
        save_preset(&dir, "Banana", &serde_json::Map::new()).unwrap();

        let presets = load_presets(&dir).unwrap();
        assert_eq!(presets[0].name, "apple");
        assert_eq!(presets[1].name, "Banana");
        assert_eq!(presets[2].name, "Zebra");
    }

    #[test]
    fn delete_preset_removes_by_case_insensitive_name() {
        let dir = temp_dir();
        save_preset(&dir, "Keep This", &serde_json::Map::new()).unwrap();
        save_preset(&dir, "Delete This", &serde_json::Map::new()).unwrap();

        let deleted = delete_preset(&dir, "delete this").unwrap();
        assert!(deleted);

        let presets = load_presets(&dir).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "Keep This");
    }

    #[test]
    fn delete_nonexistent_preset_returns_false() {
        let dir = temp_dir();
        let deleted = delete_preset(&dir, "Nonexistent").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn rename_preset_updates_name() {
        let dir = temp_dir();
        let mut filters = serde_json::Map::new();
        filters.insert("state".into(), serde_json::json!({"eq": "In Progress"}));
        save_preset(&dir, "Old Name", &filters).unwrap();

        let renamed = rename_preset(&dir, "old name", "New Name").unwrap();
        assert!(renamed);

        let presets = load_presets(&dir).unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "New Name");
        assert_eq!(presets[0].filters.get("state"), Some(&serde_json::json!({"eq": "In Progress"})));
    }

    #[test]
    fn rename_to_existing_name_fails() {
        let dir = temp_dir();
        save_preset(&dir, "Preset A", &serde_json::Map::new()).unwrap();
        save_preset(&dir, "Preset B", &serde_json::Map::new()).unwrap();

        let result = rename_preset(&dir, "Preset A", "preset b");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn rename_nonexistent_preset_returns_false() {
        let dir = temp_dir();
        let renamed = rename_preset(&dir, "Nonexistent", "New Name").unwrap();
        assert!(!renamed);
    }
}
