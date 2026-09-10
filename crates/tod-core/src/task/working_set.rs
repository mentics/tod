use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{ListWorkingSet, SortDirection, SortKey};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedWorkingSet {
    sort_key: String,
    sort_direction: String,
    tag_filter: Option<String>,
    selected_id: Option<String>,
    active_list_id: Option<String>,
}

pub fn load_working_set(config_dir: &Path) -> ListWorkingSet {
    let path = config_dir.join("task-list-working-set.json");
    let Ok(body) = fs::read_to_string(&path) else {
        return ListWorkingSet::default_sort();
    };
    let Ok(persisted) = serde_json::from_str::<PersistedWorkingSet>(&body) else {
        return ListWorkingSet::default_sort();
    };
    ListWorkingSet {
        sort_key: parse_sort_key(&persisted.sort_key),
        sort_direction: parse_sort_direction(&persisted.sort_direction),
        tag_filter: persisted.tag_filter,
        selected_id: persisted.selected_id,
        active_list_id: persisted.active_list_id,
    }
}

pub fn save_working_set(config_dir: &Path, ws: &ListWorkingSet) {
    let path = config_dir.join("task-list-working-set.json");
    let persisted = PersistedWorkingSet {
        sort_key: sort_key_name(ws.sort_key).into(),
        sort_direction: match ws.sort_direction {
            SortDirection::Asc => "asc".into(),
            SortDirection::Desc => "desc".into(),
        },
        tag_filter: ws.tag_filter.clone(),
        selected_id: ws.selected_id.clone(),
        active_list_id: ws.active_list_id.clone(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&persisted) {
        let _ = fs::create_dir_all(config_dir);
        let _ = fs::write(path, json);
    }
}

fn sort_key_name(key: SortKey) -> &'static str {
    match key {
        SortKey::InteractionTimestamp => "timestamp",
        SortKey::TreeOrder => "tree",
        SortKey::Title => "title",
        SortKey::Lifecycle => "lifecycle",
        SortKey::TicketId => "ticket",
    }
}

fn parse_sort_key(s: &str) -> SortKey {
    match s {
        "title" => SortKey::Title,
        "lifecycle" => SortKey::Lifecycle,
        "ticket" => SortKey::TicketId,
        "tree" => SortKey::TreeOrder,
        _ => SortKey::TreeOrder,
    }
}

fn parse_sort_direction(s: &str) -> SortDirection {
    match s {
        "asc" => SortDirection::Asc,
        _ => SortDirection::Desc,
    }
}
