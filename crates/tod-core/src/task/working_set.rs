use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::model::{ActorBucket, GeneratorSubtreeSort, ListWorkingSet, SortDirection, SortKey};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedGeneratorSort {
    sort_key: String,
    sort_direction: String,
    filter_query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedWorkingSet {
    sort_key: String,
    sort_direction: String,
    tag_filter: Option<String>,
    selected_id: Option<String>,
    active_list_id: Option<String>,
    #[serde(default)]
    generator_sorts: HashMap<String, PersistedGeneratorSort>,
    /// The summary strip's filter. Absent in a file an older build wrote.
    #[serde(default)]
    actor_filter: Option<ActorBucket>,
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
        pending_changes_only: false,
        actor_filter: persisted.actor_filter,
        collapsed_groups: Default::default(),
        generator_sorts: persisted
            .generator_sorts
            .into_iter()
            .map(|(id, gs)| {
                (
                    id,
                    // A generator's row button only flips its configured sort
                    // now; a key an older build cycled to would stick, so it
                    // is dropped.
                    match parse_sort_key(&gs.sort_key) {
                        SortKey::TreeOrder => GeneratorSubtreeSort {
                            sort_key: SortKey::TreeOrder,
                            sort_direction: parse_sort_direction(&gs.sort_direction),
                            filter_query: gs.filter_query,
                        },
                        _ => GeneratorSubtreeSort {
                            filter_query: gs.filter_query,
                            ..GeneratorSubtreeSort::default()
                        },
                    },
                )
            })
            .collect(),
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
        actor_filter: ws.actor_filter,
        generator_sorts: ws
            .generator_sorts
            .iter()
            .map(|(id, gs)| {
                (
                    id.clone(),
                    PersistedGeneratorSort {
                        sort_key: sort_key_name(gs.sort_key).into(),
                        sort_direction: match gs.sort_direction {
                            SortDirection::Asc => "asc".into(),
                            SortDirection::Desc => "desc".into(),
                        },
                        filter_query: gs.filter_query.clone(),
                    },
                )
            })
            .collect(),
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
        SortKey::WaitingLongest => "waiting_longest",
    }
}

fn parse_sort_key(s: &str) -> SortKey {
    match s {
        "title" => SortKey::Title,
        "lifecycle" => SortKey::Lifecycle,
        "ticket" => SortKey::TicketId,
        "tree" => SortKey::TreeOrder,
        "waiting_longest" => SortKey::WaitingLongest,
        _ => SortKey::TreeOrder,
    }
}

fn parse_sort_direction(s: &str) -> SortDirection {
    match s {
        "asc" => SortDirection::Asc,
        _ => SortDirection::Desc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_sorts_round_trip_across_save_and_load() {
        let dir =
            std::env::temp_dir().join(format!("tod-working-set-test-{}", uuid::Uuid::new_v4()));
        let mut ws = ListWorkingSet::default_sort();
        ws.generator_sorts.insert(
            "gen-1".into(),
            GeneratorSubtreeSort {
                sort_key: SortKey::TreeOrder,
                sort_direction: SortDirection::Desc,
                filter_query: "login".into(),
            },
        );
        ws.generator_sorts.insert(
            "gen-old".into(),
            GeneratorSubtreeSort {
                sort_key: SortKey::Title,
                sort_direction: SortDirection::Desc,
                filter_query: "keep".into(),
            },
        );
        save_working_set(&dir, &ws);
        let loaded = load_working_set(&dir);
        let saved = loaded
            .generator_sorts
            .get("gen-1")
            .expect("generator sort persisted");
        assert_eq!(saved.sort_key, SortKey::TreeOrder);
        assert_eq!(saved.sort_direction, SortDirection::Desc);
        assert_eq!(saved.filter_query, "login");
        // A key an older build cycled to would stick now the button only
        // flips direction: it is dropped, the filter kept.
        let old = &loaded.generator_sorts["gen-old"];
        assert_eq!(old.sort_key, SortKey::TreeOrder);
        assert_eq!(old.sort_direction, SortDirection::Asc);
        assert_eq!(old.filter_query, "keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn actor_filter_round_trips_and_an_older_file_without_it_loads() {
        let dir =
            std::env::temp_dir().join(format!("tod-working-set-test-{}", uuid::Uuid::new_v4()));
        let mut ws = ListWorkingSet::default_sort();
        ws.actor_filter = Some(ActorBucket::OnOthers);
        save_working_set(&dir, &ws);
        assert_eq!(load_working_set(&dir).actor_filter, Some(ActorBucket::OnOthers));
        ws.actor_filter = None;
        save_working_set(&dir, &ws);
        assert_eq!(load_working_set(&dir).actor_filter, None);

        // A file written before the filter existed.
        std::fs::write(
            dir.join("task-list-working-set.json"),
            r#"{"sort_key":"title","sort_direction":"asc","tag_filter":null,
                "selected_id":"n1","active_list_id":null}"#,
        )
        .unwrap();
        let old = load_working_set(&dir);
        assert_eq!(old.actor_filter, None);
        assert_eq!(old.sort_key, SortKey::Title);
        assert_eq!(old.selected_id.as_deref(), Some("n1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
