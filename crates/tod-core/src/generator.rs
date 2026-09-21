//! Generator config validation: before persisting a generator's data-source
//! configuration, validate it against the [`DataSource`] it names so invalid
//! config never reaches storage. Editing a config while a refresh is in
//! progress is allowed — the refresh in flight uses the config it was
//! started with, and the new config takes effect on the next refresh, since
//! [`set_generator_config`] never touches `last_refresh_*` fields.

use std::collections::HashMap;
use tod_integration::{DataSource, DataSourceItem, LinearDataSource, MockDataSource};

pub use tod_integration::{
    ConfigField, ConfigFieldType, ConfigSchema, CredentialRequirement, FilterPreset,
    delete_preset, load_presets, rename_preset, save_preset,
};
use tod_store::credentials::{CredentialStore, resolve_linear_api_key};
use tod_store::fleet::FleetStore;
use tod_store::outline::repos::{GeneratorRepo, NodeRepo, OutlineRepo};
use tod_store::outline::{EXTRA_CONTENT_DETAILS, OutlineMutation};
use uuid::Uuid;

const REFRESH_IN_PROGRESS: &str = "in_progress";
const REFRESH_SUCCESS: &str = "success";
const REFRESH_ERROR: &str = "error";
const REFRESH_INTERRUPTED: &str = "the refresh was interrupted before it finished";

pub const DATA_SOURCE_LINEAR: &str = "linear";
pub const DATA_SOURCE_MOCK: &str = "mock";

/// Credential key the Linear data source declares. The UI matches a missing
/// requirement against this to decide which credential prompt to open, so it
/// must stay in step with `LinearDataSource::credential_requirements` (see
/// `linear_data_source_declares_the_known_credential_key`).
pub const CREDENTIAL_LINEAR_API_KEY: &str = "linear_api_key";

/// Resolve a data source implementation by its persisted `data_source_type`.
pub fn data_source_for_type(data_source_type: &str) -> Option<Box<dyn DataSource>> {
    data_source_for_type_with_root(data_source_type, None)
}

/// Resolve a data source implementation with an optional data root for caching.
pub fn data_source_for_type_with_root(
    data_source_type: &str,
    data_root: Option<&std::path::Path>,
) -> Option<Box<dyn DataSource>> {
    match data_source_type {
        DATA_SOURCE_LINEAR => {
            if let Some(root) = data_root {
                Some(Box::new(LinearDataSource::with_data_root(root.to_path_buf())))
            } else {
                Some(Box::new(LinearDataSource::new()))
            }
        }
        DATA_SOURCE_MOCK => Some(Box::new(MockDataSource::new())),
        _ => None,
    }
}

/// All registered data source types, for UI enumeration: `(type_key, display_name, description)`.
pub fn available_data_sources() -> Vec<(&'static str, String, String)> {
    [DATA_SOURCE_LINEAR, DATA_SOURCE_MOCK]
        .into_iter()
        .map(|key| {
            let ds = data_source_for_type(key).expect("registered type key must resolve");
            (
                key,
                ds.display_name().to_string(),
                ds.description().to_string(),
            )
        })
        .collect()
}

/// The configuration form a data source expects, for UIs that render a field
/// per entry rather than asking the user to write the config JSON by hand.
pub fn config_schema_for_type(data_source_type: &str) -> Option<ConfigSchema> {
    data_source_for_type(data_source_type).map(|ds| ds.configuration_schema())
}

/// Get introspection metadata for a data source, if available.
pub fn introspection_metadata_for_type(
    data_source_type: &str,
    data_root: Option<&std::path::Path>,
) -> Option<serde_json::Value> {
    data_source_for_type_with_root(data_source_type, data_root)
        .and_then(|ds| ds.introspection_metadata())
}

/// Check if introspection cache exists for a data source.
pub fn has_introspection_cache(
    data_source_type: &str,
    data_root: Option<&std::path::Path>,
) -> bool {
    data_source_for_type_with_root(data_source_type, data_root)
        .map(|ds| ds.has_introspection_cache())
        .unwrap_or(false)
}

/// Force refresh introspection metadata for a data source.
pub fn refresh_introspection_metadata(
    data_source_type: &str,
    data_root: &std::path::Path,
    api_key: &str,
) -> Result<(), String> {
    let ds = data_source_for_type_with_root(data_source_type, Some(data_root))
        .ok_or_else(|| format!("unknown data source type: {data_source_type}"))?;
    ds.refresh_introspection(api_key)
        .map_err(|err| err.to_string())
}

/// Validate `config_json` against the named data source and persist it via
/// [`OutlineMutation::SetGeneratorConfig`], **without** refreshing. Rejects
/// unknown data source types and configs that fail
/// [`DataSource::validate_config`] or test query validation (when credentials available)
/// without enqueuing anything.
///
/// Returns `true` when this was the node's first config, meaning an initial
/// refresh is due. Saving is local and fast; refreshing reaches the network,
/// so the two are separate calls and the caller decides where the refresh
/// runs — a UI caller must run [`refresh_generator`] off its main thread.
/// See [`set_generator_config`] for the combined, fully blocking version.
pub fn save_generator_config(
    fleet: &FleetStore,
    node_id: Uuid,
    data_source_type: &str,
    config_json: &str,
) -> Result<bool, String> {
    let data_root = fleet.paths().root();
    let data_source = data_source_for_type_with_root(data_source_type, Some(data_root))
        .ok_or_else(|| format!("unknown data source type: {data_source_type}"))?;

    let config: serde_json::Value =
        serde_json::from_str(config_json).map_err(|err| format!("invalid config JSON: {err}"))?;

    // Try test query validation if credentials are available
    let credentials = resolve_credentials(data_root, data_source.as_ref());
    let missing = missing_credentials(data_source.as_ref(), &credentials);

    if missing.is_empty() {
        // We have credentials, use test query validation
        data_source
            .validate_config_with_test_query(&config, &credentials)
            .map_err(|err| err.to_string())?;
    } else {
        // No credentials, fall back to basic validation
        data_source
            .validate_config(&config)
            .map_err(|err| err.to_string())?;
    }

    let had_existing_config = fleet
        .read(move |conn| Ok(GeneratorRepo::new(conn).get_config(node_id)?.is_some()))
        .map_err(|err| err.to_string())?;

    fleet
        .enqueue_outline(OutlineMutation::SetGeneratorConfig {
            node_id,
            data_source_type: data_source_type.to_string(),
            config_json: config_json.to_string(),
        })
        .map_err(|err| err.to_string())?;
    fleet.writer().flush().map_err(|err| err.to_string())?;

    Ok(!had_existing_config)
}

/// [`save_generator_config`] plus the initial refresh it reports as due, run
/// inline. Blocks on the network for that first save, so only callers that
/// are already off a UI thread (tests, the CLI) should use it.
///
/// The very first successful save for a generator node (i.e. one with no
/// prior config) triggers an automatic initial refresh. Every save after
/// that only persists the config — refreshing again is always a separate,
/// user-triggered call to [`refresh_generator`].
pub fn set_generator_config(
    fleet: &FleetStore,
    node_id: Uuid,
    data_source_type: &str,
    config_json: &str,
) -> Result<(), String> {
    if save_generator_config(fleet, node_id, data_source_type, config_json)? {
        // The config save itself has already succeeded; a failed initial
        // refresh (e.g. blocked on a missing credential) is recorded on the
        // generator's refresh status by `refresh_generator` itself and does
        // not undo or fail the save.
        let _ = refresh_generator(fleet, node_id);
    }
    Ok(())
}

/// A managed node as it currently exists in storage, for diffing against a
/// freshly fetched [`DataSourceItem`].
struct ExistingManaged {
    node_id: Uuid,
    title: String,
    tags: Vec<String>,
    body: String,
    user_modified_fields: Vec<String>,
}

/// Resolve a credential value for one of a data source's declared
/// [`CredentialRequirement`]s. Only `linear_api_key` is backed by real
/// storage today; unknown keys resolve to `None` (the data source itself
/// decides whether that is fatal, via [`DataSource::fetch`]).
fn resolve_credential(
    store: &CredentialStore,
    requirement: &CredentialRequirement,
) -> Option<String> {
    match requirement.key.as_str() {
        CREDENTIAL_LINEAR_API_KEY => resolve_linear_api_key(store),
        _ => None,
    }
}

fn resolve_credentials(
    data_root: &std::path::Path,
    data_source: &dyn DataSource,
) -> HashMap<String, String> {
    let store = CredentialStore::from_data_root(data_root);
    data_source
        .credential_requirements()
        .iter()
        .filter_map(|req| resolve_credential(&store, req).map(|value| (req.key.clone(), value)))
        .collect()
}

/// Credential requirements a data source declares that have no resolved
/// value in `credentials`. Checking this before a fetch lets the caller
/// prompt for what's missing instead of spending a network round trip to
/// learn the same thing from an [`tod_integration::DataSourceError::Auth`].
pub fn missing_credentials(
    data_source: &dyn DataSource,
    credentials: &HashMap<String, String>,
) -> Vec<CredentialRequirement> {
    data_source
        .credential_requirements()
        .into_iter()
        .filter(|req| !credentials.contains_key(&req.key))
        .collect()
}

/// Why a generator refresh did not run.
///
/// [`RefreshError::MissingCredentials`] is split out from the rest so callers
/// can act on it instead of parsing the message: the UI prompts for the named
/// credentials and retries. `Display` renders both variants as the plain
/// message that is also recorded on the generator's refresh status, so a
/// caller that only wants to report the failure can still use `to_string`.
#[derive(Debug, Clone)]
pub enum RefreshError {
    /// Credentials the data source declared have no resolved value. Nothing
    /// was fetched, and the generator's existing managed nodes are untouched.
    MissingCredentials(Vec<CredentialRequirement>),
    /// Anything else: an absent or unknown configuration, or a failed fetch.
    Other(String),
}

impl RefreshError {
    /// The missing requirements, or an empty slice for any other failure.
    pub fn missing_credentials(&self) -> &[CredentialRequirement] {
        match self {
            Self::MissingCredentials(missing) => missing,
            Self::Other(_) => &[],
        }
    }

    /// Whether this failure is a blocked refresh waiting on the one
    /// credential the UI has a prompt for.
    pub fn needs_linear_api_key(&self) -> bool {
        self.missing_credentials()
            .iter()
            .any(|req| req.key == CREDENTIAL_LINEAR_API_KEY)
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCredentials(missing) => {
                let labels = missing
                    .iter()
                    .map(|req| req.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "missing credentials: {labels}")
            }
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl From<String> for RefreshError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

/// Refresh a generator node: fetch from its configured data source, resolve
/// credentials, and reconcile the result against existing managed nodes.
/// Looks up the data source implementation and credentials for the caller;
/// see [`refresh_generator_with`] for the injectable version used in tests.
///
/// Blocked before any fetch is attempted if a required credential (e.g. a
/// Linear API key) has no resolved value, and reported as
/// [`RefreshError::MissingCredentials`] so the caller can prompt for it
/// (reusing the same credential prompt the Linear ticket import flow uses,
/// see `tod-ui`'s `views::task_list::credential_prompt`) and retry once it is
/// supplied.
pub fn refresh_generator(fleet: &FleetStore, node_id: Uuid) -> Result<Vec<Uuid>, RefreshError> {
    let config = {
        let node_id = node_id;
        fleet
            .read(move |conn| GeneratorRepo::new(conn).get_config(node_id))
            .map_err(|err| err.to_string())?
    };
    let Some(config) = config else {
        return Err(RefreshError::Other(
            "node has no generator configuration".into(),
        ));
    };
    let data_root = fleet.paths().root();
    let data_source = data_source_for_type_with_root(&config.data_source_type, Some(data_root))
        .ok_or_else(|| {
            RefreshError::Other(format!(
                "unknown data source type: {}",
                config.data_source_type
            ))
        })?;
    let credentials = resolve_credentials(data_root, data_source.as_ref());

    let missing = missing_credentials(data_source.as_ref(), &credentials);
    if !missing.is_empty() {
        let err = RefreshError::MissingCredentials(missing);
        set_refresh_error(fleet, node_id, &err.to_string())?;
        return Err(err);
    }

    // The fetch is the one step that can outlast any sane wait (a stalled
    // connection, a rate-limit back-off), so it runs under a deadline: past
    // it the refresh is recorded as failed and the fetch thread is abandoned,
    // instead of the row reading "refreshing…" for as long as the app is up.
    let data_source: std::sync::Arc<dyn DataSource> = std::sync::Arc::from(data_source);
    refresh_with_fetch(fleet, node_id, move |config| {
        run_with_deadline(REFRESH_FETCH_DEADLINE, move || {
            data_source
                .fetch(&config, &credentials)
                .map_err(|err| err.to_string())
        })
        .and_then(|fetched| fetched)
    })
    .map_err(RefreshError::Other)
}

/// How long a refresh waits on its data source before giving up.
const REFRESH_FETCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);

/// Run `work` on its own thread and wait at most `deadline` for it. On a
/// timeout the thread is left to finish on its own and its result is dropped;
/// a panic in `work` comes back as an error rather than unwinding the caller.
fn run_with_deadline<T: Send + 'static>(
    deadline: std::time::Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("generator-fetch".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|err| format!("could not start the fetch: {err}"))?;
    match rx.recv_timeout(deadline) {
        Ok(value) => Ok(value),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "the data source did not answer within {} seconds",
            deadline.as_secs()
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("the fetch stopped unexpectedly".into())
        }
    }
}

/// Reconcile a generator node's managed subtree against a fetched item tree.
///
/// Refresh is atomic: mutations are only enqueued after `fetch` succeeds in
/// full, so a failed fetch leaves existing managed nodes untouched. A
/// trigger that arrives while this process is already refreshing the node is
/// silently ignored rather than erroring, so concurrent triggers collapse
/// onto the one in flight (see [`InFlightRefresh`]). Fields a user has
/// edited on a copied-out/managed node (tracked in
/// [`tod_store::outline::repos::generator::ManagedNodeLink::user_modified_fields`])
/// are preserved rather than overwritten by the fetched value. Tags are
/// written directly onto each node's `node_fields.tags`; there is no
/// separate tag-pool table to update — anything that lists tags across a
/// list already derives them by scanning nodes, so a new tag becomes visible
/// there as soon as it's set here.
pub fn refresh_generator_with(
    fleet: &FleetStore,
    node_id: Uuid,
    data_source: &dyn DataSource,
    credentials: &HashMap<String, String>,
) -> Result<Vec<Uuid>, String> {
    refresh_with_fetch(fleet, node_id, |config| {
        data_source
            .fetch(&config, credentials)
            .map_err(|err| err.to_string())
    })
}

/// [`refresh_generator_with`], with the fetch itself supplied by the caller
/// so it can decide where and for how long the fetch runs.
fn refresh_with_fetch(
    fleet: &FleetStore,
    node_id: Uuid,
    fetch: impl FnOnce(serde_json::Value) -> Result<Vec<DataSourceItem>, String>,
) -> Result<Vec<Uuid>, String> {
    // Whether a refresh is running is a fact about this process, not the
    // database: a persisted `in_progress` with no claim behind it was left by
    // a process that died mid-refresh, and must not block the next one.
    let Some(_claim) = InFlightRefresh::claim(fleet, node_id) else {
        tracing::info!(%node_id, "generator refresh ignored: one is already in flight");
        return Ok(Vec::new());
    };

    let loaded = fleet
        .read(move |conn| {
            let gen_repo = GeneratorRepo::new(conn);
            let Some(config) = gen_repo.get_config(node_id)? else {
                return Ok(None);
            };
            let Some(entry) = OutlineRepo::new(conn).get_entry(node_id)? else {
                return Ok(None);
            };
            let node_repo = NodeRepo::new(conn);
            let links = gen_repo.links_for_generator(node_id)?;
            let mut existing = HashMap::new();
            for link in links {
                if !gen_repo.is_managed(link.node_id)? {
                    // Copied-out nodes share the generator_node_id/external_id of
                    // their source but are no longer part of the managed tree —
                    // reconciliation must not treat them as managed items.
                    continue;
                }
                let Some(node) = node_repo.get(link.node_id)? else {
                    continue;
                };
                let tags = node_repo.get_tags(link.node_id)?;
                let body = node_repo
                    .get_extra_content(link.node_id, EXTRA_CONTENT_DETAILS)?
                    .unwrap_or_default();
                existing.insert(
                    link.external_id.clone(),
                    ExistingManaged {
                        node_id: link.node_id,
                        title: node.title,
                        tags,
                        body,
                        user_modified_fields: link.user_modified_fields,
                    },
                );
            }
            Ok(Some((config, entry.list_id, existing)))
        })
        .map_err(|err| err.to_string())?;

    let Some((config, list_id, existing)) = loaded else {
        // No config or no outline entry — nothing to do.
        return Ok(Vec::new());
    };

    fleet
        .enqueue_outline(OutlineMutation::SetRefreshStatus {
            node_id,
            status: REFRESH_IN_PROGRESS.into(),
            error: None,
        })
        .map_err(|err| err.to_string())?;
    fleet.writer().flush().map_err(|err| err.to_string())?;

    // From here the row says `in_progress`, so every way out has to replace
    // it: success does below, and any failure is recorded here.
    tracing::info!(%node_id, source = %config.data_source_type, "generator refresh started");
    let started = std::time::Instant::now();
    let result = fetch_and_reconcile(fleet, node_id, list_id, &config, &existing, fetch);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(_) => tracing::info!(%node_id, elapsed_ms, "generator refresh finished"),
        Err(msg) => {
            tracing::error!(%node_id, elapsed_ms, error = %msg, "generator refresh failed");
            if let Err(err) = set_refresh_error(fleet, node_id, msg) {
                tracing::error!(%node_id, error = %err, "could not record the failed refresh");
            }
        }
    }
    result
}

/// A refresh running in this process. Claims are what collapse concurrent
/// triggers onto the one in flight; the persisted `in_progress` status is for
/// display only. Dropping the claim releases it, and a claim dropped by a
/// panic marks the refresh failed so the row does not read "refreshing…"
/// until the app restarts.
struct InFlightRefresh<'a> {
    fleet: &'a FleetStore,
    node_id: Uuid,
}

static IN_FLIGHT_REFRESHES: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
    std::sync::LazyLock::new(Default::default);

impl<'a> InFlightRefresh<'a> {
    /// `None` when this process is already refreshing `node_id`.
    fn claim(fleet: &'a FleetStore, node_id: Uuid) -> Option<Self> {
        let claimed = IN_FLIGHT_REFRESHES
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(node_id);
        // Built only once claimed: dropping one releases the claim.
        claimed.then(|| Self { fleet, node_id })
    }
}

impl Drop for InFlightRefresh<'_> {
    fn drop(&mut self) {
        IN_FLIGHT_REFRESHES
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .remove(&self.node_id);
        if std::thread::panicking() {
            tracing::error!(node_id = %self.node_id, "generator refresh panicked");
            let _ = set_refresh_error(self.fleet, self.node_id, REFRESH_INTERRUPTED);
        }
    }
}

/// Mark every generator still recorded as `in_progress` as failed. Refreshes
/// run only inside the app, so at launch any such row was orphaned by a
/// process that exited mid-refresh; left alone it would show "refreshing…"
/// forever. Call once at app start, before anything can start a refresh.
pub fn clear_interrupted_refreshes(fleet: &FleetStore) -> Result<usize, String> {
    let orphaned = fleet
        .read(|conn| GeneratorRepo::new(conn).nodes_with_refresh_status(REFRESH_IN_PROGRESS))
        .map_err(|err| err.to_string())?;
    for node_id in &orphaned {
        tracing::warn!(%node_id, "generator refresh left in progress by an earlier run");
        set_refresh_error(fleet, *node_id, REFRESH_INTERRUPTED)?;
    }
    Ok(orphaned.len())
}

fn fetch_and_reconcile(
    fleet: &FleetStore,
    node_id: Uuid,
    list_id: Uuid,
    config: &tod_store::outline::repos::GeneratorConfig,
    existing: &HashMap<String, ExistingManaged>,
    fetch: impl FnOnce(serde_json::Value) -> Result<Vec<DataSourceItem>, String>,
) -> Result<Vec<Uuid>, String> {
    let config_value: serde_json::Value = serde_json::from_str(&config.config_json)
        .map_err(|err| format!("invalid stored config JSON: {err}"))?;

    let items = fetch(config_value)?;

    let mut mutations = Vec::new();
    let mut visited = std::collections::HashSet::new();
    reconcile_level(
        &items,
        node_id,
        list_id,
        node_id,
        &config.data_source_type,
        existing,
        &mut visited,
        &mut mutations,
    );
    for (external_id, existing_item) in existing {
        if !visited.contains(external_id) {
            mutations.push(OutlineMutation::DeleteManagedNode {
                node_id: existing_item.node_id,
            });
            // The external item is gone from the source — any copies of it
            // living outside the generator subtree become stale; they keep
            // their title/content but stop receiving refresh updates.
            mutations.push(OutlineMutation::ClearStaleCopyLinks {
                generator_node_id: node_id,
                external_id: external_id.clone(),
            });
        }
    }
    let updated_copy_ids = collect_linked_copy_updates(fleet, node_id, &items, &mut mutations)?;

    mutations.push(OutlineMutation::SetRefreshStatus {
        node_id,
        status: REFRESH_SUCCESS.into(),
        error: None,
    });

    for mutation in mutations {
        fleet
            .enqueue_outline(mutation)
            .map_err(|err| err.to_string())?;
    }
    fleet.writer().flush().map_err(|err| err.to_string())?;
    Ok(updated_copy_ids)
}

/// Push field updates for every already-copied-out (non-managed, linked) node
/// whose external item is still present in this refresh's results. Each
/// copy's individual `user_modified_fields` are respected — locally edited
/// fields are left untouched, descendant linked nodes are updated
/// independently of their ancestor.
fn collect_linked_copy_updates(
    fleet: &FleetStore,
    generator_node_id: Uuid,
    items: &[DataSourceItem],
    mutations: &mut Vec<OutlineMutation>,
) -> Result<Vec<Uuid>, String> {
    let mut updated = Vec::new();
    for item in items {
        let links = fleet
            .read(|conn| {
                GeneratorRepo::new(conn).copy_links_for(generator_node_id, &item.external_id)
            })
            .map_err(|err| err.to_string())?;
        for link in links {
            let dirty = |field: &str| link.user_modified_fields.iter().any(|f| f == field);
            let title = (!dirty("title")).then(|| item.title.clone());
            let tags = (!dirty("tags")).then(|| item.tags.clone());
            let body = (!dirty("body")).then(|| item.body.clone());
            if title.is_some() || tags.is_some() || body.is_some() {
                mutations.push(OutlineMutation::RefreshLinkedCopy {
                    node_id: link.node_id,
                    title,
                    tags,
                    body,
                });
                updated.push(link.node_id);
            }
        }
        updated.extend(collect_linked_copy_updates(
            fleet,
            generator_node_id,
            &item.children,
            mutations,
        )?);
    }
    Ok(updated)
}

#[allow(clippy::too_many_arguments)]
fn reconcile_level(
    items: &[DataSourceItem],
    parent_id: Uuid,
    list_id: Uuid,
    generator_node_id: Uuid,
    source_type: &str,
    existing: &HashMap<String, ExistingManaged>,
    visited: &mut std::collections::HashSet<String>,
    mutations: &mut Vec<OutlineMutation>,
) {
    for item in items {
        visited.insert(item.external_id.clone());
        let node_id = if let Some(existing_item) = existing.get(&item.external_id) {
            let keep = |field: &str, current: &str, fetched: &str| {
                if existing_item
                    .user_modified_fields
                    .iter()
                    .any(|f| f == field)
                {
                    current.to_string()
                } else {
                    fetched.to_string()
                }
            };
            let title = keep("title", &existing_item.title, &item.title);
            let tags = if existing_item
                .user_modified_fields
                .iter()
                .any(|f| f == "tags")
            {
                existing_item.tags.clone()
            } else {
                item.tags.clone()
            };
            let body = keep("body", &existing_item.body, &item.body);
            mutations.push(OutlineMutation::UpdateManagedNode {
                node_id: existing_item.node_id,
                title,
                tags,
                body,
                metadata: item.metadata.clone(),
            });
            existing_item.node_id
        } else {
            let new_node_id = Uuid::new_v4();
            mutations.push(OutlineMutation::CreateManagedNode {
                node_id: Some(new_node_id),
                list_id,
                parent_id,
                title: item.title.clone(),
                external_id: item.external_id.clone(),
                source_type: source_type.to_string(),
                generator_node_id,
                tags: item.tags.clone(),
                body: item.body.clone(),
                metadata: item.metadata.clone(),
            });
            new_node_id
        };
        reconcile_level(
            &item.children,
            node_id,
            list_id,
            generator_node_id,
            source_type,
            existing,
            visited,
            mutations,
        );
    }
}

fn set_refresh_error(fleet: &FleetStore, node_id: Uuid, message: &str) -> Result<(), String> {
    fleet
        .enqueue_outline(OutlineMutation::SetRefreshStatus {
            node_id,
            status: REFRESH_ERROR.into(),
            error: Some(message.to_string()),
        })
        .map_err(|err| err.to_string())?;
    fleet.writer().flush().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tod_store::fleet::FleetStore;
    use tod_store::fleet::schema::open_read_connection;
    use tod_store::outline::CreatePosition;
    use tod_store::outline::repos::GeneratorRepo;
    use tod_store::outline::types::Capability;

    fn setup() -> (std::path::PathBuf, FleetStore) {
        let root = std::env::temp_dir().join(format!("tod-gen-core-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let store = FleetStore::open(&root).unwrap();
        store
            .enqueue_outline(OutlineMutation::CreateList {
                slug: "gen-core-test".into(),
                title: "Gen Core Test".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        (root, store)
    }

    fn create_generator_node(fleet: &FleetStore, list_id: Uuid) -> Uuid {
        let node_id = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node_id),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Generator".into(),
            })
            .unwrap();
        fleet
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id,
                capabilities: vec![Capability::Generator],
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        node_id
    }

    fn read_config(
        root: &std::path::Path,
        node_id: Uuid,
    ) -> Option<tod_store::outline::repos::GeneratorConfig> {
        let conn = open_read_connection(&root.join("tod.db")).unwrap();
        GeneratorRepo::new(&conn).get_config(node_id).unwrap()
    }

    #[test]
    fn invalid_config_rejected_and_not_persisted() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        let result =
            set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"invalid": true}"#);
        assert!(result.is_err());
        assert!(read_config(&root, node_id).is_none());

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn valid_config_accepted_and_persisted() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"query": "foo"}"#).unwrap();

        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.config_json, r#"{"query": "foo"}"#);

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn edit_config_during_refresh_used_on_next_refresh() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"query": "old"}"#).unwrap();
        fleet
            .enqueue_outline(OutlineMutation::SetRefreshStatus {
                node_id,
                status: "in_progress".into(),
                error: None,
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"query": "new"}"#).unwrap();

        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.config_json, r#"{"query": "new"}"#);
        assert_eq!(config.last_refresh_status.as_deref(), Some("in_progress"));

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_data_source_type_rejected() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        let result = set_generator_config(&fleet, node_id, "nonexistent", "{}");
        assert!(result.is_err());

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    // ── Refresh / reconciliation ─────────────────────────────────────────

    fn managed_children(fleet: &FleetStore, list_id: Uuid, parent_id: Uuid) -> Vec<(Uuid, String)> {
        fleet
            .flatten_outline(list_id)
            .unwrap()
            .into_iter()
            .filter(|row| row.parent_id == Some(parent_id))
            .map(|row| (row.node.id, row.node.title))
            .collect()
    }

    fn mark_user_modified(root: &std::path::Path, node_id: Uuid, fields: &[&str]) {
        let conn = tod_store::fleet::schema::open_writer_connection(&root.join("tod.db")).unwrap();
        let fields: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
        GeneratorRepo::new(&conn)
            .update_user_modified_fields(node_id, &fields)
            .unwrap();
    }

    fn item(external_id: &str, title: &str, children: Vec<DataSourceItem>) -> DataSourceItem {
        // Mirror Linear adapter behavior: title includes identifier prefix
        let prefixed_title = format!("{}: {}", external_id, title);
        DataSourceItem {
            external_id: external_id.into(),
            title: prefixed_title,
            tags: vec![],
            body: String::new(),
            metadata: None,
            children,
        }
    }

    #[test]
    fn refresh_creates_new_managed_nodes() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "First", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        let children = managed_children(&fleet, list_id, node_id);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].1, "EXT-1: First");

        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.last_refresh_status.as_deref(), Some(REFRESH_SUCCESS));

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_creates_child_items_as_tree() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item(
            "EXT-1",
            "Parent",
            vec![item("EXT-2", "Child", vec![])],
        )]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        let top = managed_children(&fleet, list_id, node_id);
        assert_eq!(top.len(), 1);
        let sub = managed_children(&fleet, list_id, top[0].0);
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].1, "EXT-2: Child");

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_updates_existing_item_title() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Old title", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let first = managed_children(&fleet, list_id, node_id);

        ds.set_items(vec![item("EXT-1", "New title", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let second = managed_children(&fleet, list_id, node_id);

        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, first[0].0, "same node reused across refreshes");
        assert_eq!(second[0].1, "EXT-1: New title");

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_removes_items_no_longer_returned() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Gone soon", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        assert_eq!(managed_children(&fleet, list_id, node_id).len(), 1);

        ds.set_items(vec![]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        assert_eq!(managed_children(&fleet, list_id, node_id).len(), 0);

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_preserves_user_modified_field() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Original", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let managed_id = managed_children(&fleet, list_id, node_id)[0].0;

        mark_user_modified(&root, managed_id, &["title"]);

        ds.set_items(vec![item("EXT-1", "From source", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        let children = managed_children(&fleet, list_id, node_id);
        assert_eq!(
            children[0].1, "EXT-1: Original",
            "user-edited title must survive refresh"
        );

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn greyed_out_state_survives_a_config_change_refresh() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Original", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let managed_id = managed_children(&fleet, list_id, node_id)[0].0;

        let outside_parent = tod_store::outline::OutlineMutation::CreateNode {
            node_id: None,
            list_id,
            parent_id: None,
            anchor_id: None,
            position: tod_store::outline::CreatePosition::Below,
            title: "Outside".into(),
        };
        fleet.enqueue_outline(outside_parent).unwrap();
        fleet.writer().flush().unwrap();
        let outside_id = fleet
            .read(|conn| {
                let outline = tod_store::outline::repos::OutlineRepo::new(conn);
                Ok(outline
                    .list_for_list(list_id)
                    .unwrap()
                    .into_iter()
                    .find(|e| e.parent_id.is_none() && e.node_id != node_id)
                    .unwrap()
                    .node_id)
            })
            .unwrap();

        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::PasteManagedNodeCopy {
                source_node_id: managed_id,
                list_id,
                parent_id: Some(outside_id),
                ordinal: 0,
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        fleet
            .read(|conn| {
                let gen_repo = tod_store::outline::repos::GeneratorRepo::new(conn);
                assert!(gen_repo.is_greyed_out(managed_id).unwrap());
                Ok(())
            })
            .unwrap();

        // Config change / rebuild: same external_id still returned, same node id reused.
        ds.set_items(vec![item("EXT-1", "Updated from source", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        fleet
            .read(|conn| {
                let gen_repo = tod_store::outline::repos::GeneratorRepo::new(conn);
                assert!(
                    gen_repo.is_greyed_out(managed_id).unwrap(),
                    "greyed-out state must survive a config-change rebuild that still returns the same external id"
                );
                Ok(())
            })
            .unwrap();

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_clears_link_on_copy_when_external_item_is_removed_from_source() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Original", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let managed_id = managed_children(&fleet, list_id, node_id)[0].0;

        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "Outside".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        let outside_id = fleet
            .read(|conn| {
                let outline = tod_store::outline::repos::OutlineRepo::new(conn);
                Ok(outline
                    .list_for_list(list_id)
                    .unwrap()
                    .into_iter()
                    .find(|e| e.parent_id.is_none() && e.node_id != node_id)
                    .unwrap()
                    .node_id)
            })
            .unwrap();

        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::PasteManagedNodeCopy {
                source_node_id: managed_id,
                list_id,
                parent_id: Some(outside_id),
                ordinal: 0,
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        let copy_id = fleet
            .read(|conn| {
                let outline = tod_store::outline::repos::OutlineRepo::new(conn);
                Ok(outline
                    .list_for_list(list_id)
                    .unwrap()
                    .into_iter()
                    .find(|e| e.parent_id == Some(outside_id))
                    .unwrap()
                    .node_id)
            })
            .unwrap();

        fleet
            .read(|conn| {
                let gen_repo = tod_store::outline::repos::GeneratorRepo::new(conn);
                assert!(gen_repo.get_link(copy_id).unwrap().is_some());
                Ok(())
            })
            .unwrap();

        ds.set_items(vec![]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        fleet
            .read(|conn| {
                let gen_repo = tod_store::outline::repos::GeneratorRepo::new(conn);
                let node_repo = tod_store::outline::repos::NodeRepo::new(conn);
                assert!(
                    gen_repo.get_link(copy_id).unwrap().is_none(),
                    "copy should lose its link once the external item is gone from the source"
                );
                assert!(
                    node_repo.get(copy_id).unwrap().is_some(),
                    "copy remains as a plain normal node"
                );
                Ok(())
            })
            .unwrap();

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    fn make_copy(
        fleet: &FleetStore,
        list_id: Uuid,
        source_node_id: Uuid,
        target_parent_id: Uuid,
    ) -> Uuid {
        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::PasteManagedNodeCopy {
                source_node_id,
                list_id,
                parent_id: Some(target_parent_id),
                ordinal: 0,
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet
            .read(|conn| {
                let outline = tod_store::outline::repos::OutlineRepo::new(conn);
                Ok(outline
                    .list_for_list(list_id)
                    .unwrap()
                    .into_iter()
                    .find(|e| e.parent_id == Some(target_parent_id))
                    .unwrap()
                    .node_id)
            })
            .unwrap()
    }

    fn make_outside_parent(fleet: &FleetStore, list_id: Uuid, exclude: &[Uuid]) -> Uuid {
        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::CreateNode {
                node_id: None,
                list_id,
                parent_id: None,
                anchor_id: None,
                position: tod_store::outline::CreatePosition::Below,
                title: "Outside".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet
            .read(|conn| {
                let outline = tod_store::outline::repos::OutlineRepo::new(conn);
                Ok(outline
                    .list_for_list(list_id)
                    .unwrap()
                    .into_iter()
                    .find(|e| e.parent_id.is_none() && !exclude.contains(&e.node_id))
                    .unwrap()
                    .node_id)
            })
            .unwrap()
    }

    #[test]
    fn refresh_returns_ids_of_updated_copies_for_session_badge() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Original", vec![])]);
        let ids = refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        assert!(ids.is_empty(), "no copies yet, nothing to badge");
        let managed_id = managed_children(&fleet, list_id, node_id)[0].0;

        let outside_id = make_outside_parent(&fleet, list_id, &[node_id]);
        let copy_id = make_copy(&fleet, list_id, managed_id, outside_id);

        ds.set_items(vec![item("EXT-1", "Updated", vec![])]);
        let ids = refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        assert_eq!(ids, vec![copy_id]);

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_updates_unmodified_fields_on_copied_out_node() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Original", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let managed_id = managed_children(&fleet, list_id, node_id)[0].0;

        let outside_id = make_outside_parent(&fleet, list_id, &[node_id]);
        let copy_id = make_copy(&fleet, list_id, managed_id, outside_id);

        ds.set_items(vec![item("EXT-1", "Updated from source", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        fleet
            .read(|conn| {
                let node_repo = tod_store::outline::repos::NodeRepo::new(conn);
                let copy = node_repo.get(copy_id).unwrap().unwrap();
                assert_eq!(copy.title, "EXT-1: Updated from source");
                Ok(())
            })
            .unwrap();

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_preserves_dirty_field_on_copied_out_node_but_updates_others() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Original", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let managed_id = managed_children(&fleet, list_id, node_id)[0].0;

        let outside_id = make_outside_parent(&fleet, list_id, &[node_id]);
        let copy_id = make_copy(&fleet, list_id, managed_id, outside_id);

        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdateNodeTitle {
                node_id: copy_id,
                title: "User-edited title".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        ds.set_items(vec![item("EXT-1", "Updated from source", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        fleet
            .read(|conn| {
                let node_repo = tod_store::outline::repos::NodeRepo::new(conn);
                let copy = node_repo.get(copy_id).unwrap().unwrap();
                assert_eq!(
                    copy.title, "User-edited title",
                    "dirty title must not be overwritten"
                );
                Ok(())
            })
            .unwrap();

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_updates_descendant_linked_copies_independently() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item(
            "EXT-1",
            "Parent",
            vec![item("EXT-2", "Child", vec![])],
        )]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        let parent_managed_id = managed_children(&fleet, list_id, node_id)[0].0;
        let child_managed_id = managed_children(&fleet, list_id, parent_managed_id)[0].0;

        let outside_id = make_outside_parent(&fleet, list_id, &[node_id]);
        let parent_copy_id = make_copy(&fleet, list_id, parent_managed_id, outside_id);
        let child_copy_id = fleet
            .read(|conn| {
                let outline = tod_store::outline::repos::OutlineRepo::new(conn);
                Ok(outline
                    .list_for_list(list_id)
                    .unwrap()
                    .into_iter()
                    .find(|e| e.parent_id == Some(parent_copy_id))
                    .unwrap()
                    .node_id)
            })
            .unwrap();
        let _ = child_managed_id;

        fleet
            .enqueue_outline(tod_store::outline::OutlineMutation::UpdateNodeTitle {
                node_id: parent_copy_id,
                title: "User-edited parent".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();

        ds.set_items(vec![item(
            "EXT-1",
            "Updated parent",
            vec![item("EXT-2", "Updated child", vec![])],
        )]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        fleet
            .read(|conn| {
                let node_repo = tod_store::outline::repos::NodeRepo::new(conn);
                let parent_copy = node_repo.get(parent_copy_id).unwrap().unwrap();
                let child_copy = node_repo.get(child_copy_id).unwrap().unwrap();
                assert_eq!(
                    parent_copy.title, "User-edited parent",
                    "parent's dirty title stays put"
                );
                assert_eq!(
                    child_copy.title, "EXT-2: Updated child",
                    "child updates independently of its dirty parent"
                );
                Ok(())
            })
            .unwrap();

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_fails_atomically_on_fetch_error() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new()
            .with_items(vec![item("EXT-1", "Should not persist", vec![])])
            .with_error(tod_integration::DataSourceError::Fetch("boom".into()));
        let result = refresh_generator_with(&fleet, node_id, &ds, &HashMap::new());
        assert!(result.is_err());

        assert_eq!(managed_children(&fleet, list_id, node_id).len(), 0);
        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.last_refresh_status.as_deref(), Some(REFRESH_ERROR));

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn first_config_save_triggers_initial_refresh() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let config = read_config(&root, node_id).unwrap();
        assert_eq!(
            config.last_refresh_status.as_deref(),
            Some(REFRESH_SUCCESS),
            "first save should have triggered an automatic refresh"
        );

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_reports_initial_refresh_due_without_running_it() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        let refresh_due = save_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();
        assert!(refresh_due, "the first save leaves an initial refresh due");

        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.config_json, "{}", "config is persisted by the save");
        assert_eq!(
            config.last_refresh_status, None,
            "save must not refresh — the caller runs it off its own thread"
        );

        let refresh_due =
            save_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"query":"x"}"#).unwrap();
        assert!(!refresh_due, "only the very first save is owed a refresh");

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_rejects_invalid_config_without_persisting() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        let result = save_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"invalid":true}"#);
        assert!(result.is_err(), "invalid config must be rejected");
        assert!(
            read_config(&root, node_id).is_none(),
            "a rejected config must not reach storage"
        );

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn subsequent_config_save_does_not_trigger_refresh() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);

        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();
        let after_first = read_config(&root, node_id).unwrap().last_refresh_at;
        assert!(after_first.is_some(), "first save should have refreshed");

        std::thread::sleep(std::time::Duration::from_millis(5));
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, r#"{"query":"changed"}"#).unwrap();

        let after_second = read_config(&root, node_id).unwrap().last_refresh_at;
        assert_eq!(
            after_first, after_second,
            "subsequent saves must not auto-refresh"
        );

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    /// The UI decides which prompt a blocked refresh maps to by matching the
    /// missing requirement's key against [`CREDENTIAL_LINEAR_API_KEY`]. If the
    /// data source ever renamed its key, that match would silently stop firing
    /// and the user would be back to an error with no way to act on it.
    #[test]
    fn linear_data_source_declares_the_known_credential_key() {
        let keys: Vec<String> = LinearDataSource::new()
            .credential_requirements()
            .into_iter()
            .map(|req| req.key)
            .collect();
        assert_eq!(keys, vec![CREDENTIAL_LINEAR_API_KEY.to_string()]);
    }

    /// A blocked refresh has to be distinguishable from every other failure
    /// without parsing the message, while still rendering as that message.
    #[test]
    fn missing_credential_refresh_error_carries_requirements_and_renders_message() {
        let err = RefreshError::MissingCredentials(vec![CredentialRequirement {
            key: CREDENTIAL_LINEAR_API_KEY.into(),
            label: "Linear API key".into(),
        }]);
        assert_eq!(err.to_string(), "missing credentials: Linear API key");
        assert_eq!(err.missing_credentials().len(), 1);
        assert!(err.needs_linear_api_key());

        let other = RefreshError::Other("boom".into());
        assert_eq!(other.to_string(), "boom");
        assert!(other.missing_credentials().is_empty());
        assert!(!other.needs_linear_api_key());

        // A data source can declare a credential nothing knows how to
        // collect; that must not be mistaken for the Linear key.
        let unknown = RefreshError::MissingCredentials(vec![CredentialRequirement {
            key: "some_other_key".into(),
            label: "Some other key".into(),
        }]);
        assert!(!unknown.needs_linear_api_key());
    }

    #[test]
    fn missing_credentials_reports_unresolved_requirements() {
        let ds = MockDataSource::new().with_credentials(vec![CredentialRequirement {
            key: "linear_api_key".into(),
            label: "Linear API key".into(),
        }]);

        let missing = missing_credentials(&ds, &HashMap::new());
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].key, "linear_api_key");

        let mut present = HashMap::new();
        present.insert("linear_api_key".to_string(), "secret".to_string());
        assert!(missing_credentials(&ds, &present).is_empty());
    }

    #[test]
    fn refresh_blocked_when_credential_missing() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new()
            .with_credentials(vec![CredentialRequirement {
                key: "linear_api_key".into(),
                label: "Linear API key".into(),
            }])
            .with_items(vec![item("EXT-1", "Should not appear", vec![])]);

        let missing = missing_credentials(&ds, &HashMap::new());
        assert_eq!(
            missing.len(),
            1,
            "refresh should be blocked: credential missing"
        );
        assert_eq!(
            ds.fetch_count(),
            0,
            "fetch must not run before the credential check"
        );

        let mut present = HashMap::new();
        present.insert("linear_api_key".to_string(), "secret".to_string());
        assert!(missing_credentials(&ds, &present).is_empty());
        refresh_generator_with(&fleet, node_id, &ds, &present).unwrap();
        assert_eq!(
            managed_children(&fleet, list_id, node_id).len(),
            1,
            "refresh proceeds once credentials are present"
        );

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refresh_ignored_while_already_in_flight() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Ignored", vec![])]);
        let claim = InFlightRefresh::claim(&fleet, node_id).unwrap();
        let result = refresh_generator_with(&fleet, node_id, &ds, &HashMap::new());
        assert!(result.is_ok());
        assert_eq!(managed_children(&fleet, list_id, node_id).len(), 0);
        assert_eq!(
            ds.fetch_count(),
            0,
            "fetch must not run while a refresh is in flight"
        );

        drop(claim);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        assert_eq!(
            managed_children(&fleet, list_id, node_id).len(),
            1,
            "the claim is released once the refresh in flight ends"
        );

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deadline_returns_the_work_when_it_finishes_in_time() {
        let result = run_with_deadline(std::time::Duration::from_secs(5), || 7);
        assert_eq!(result, Ok(7));
    }

    #[test]
    fn deadline_gives_up_on_work_that_hangs() {
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let result = run_with_deadline(std::time::Duration::from_millis(50), move || {
            let _ = hold.recv();
        });
        assert!(result.unwrap_err().contains("did not answer"));
        drop(release);
    }

    #[test]
    fn deadline_reports_work_that_panics() {
        let result = run_with_deadline(std::time::Duration::from_secs(5), || -> u8 {
            panic!("boom")
        });
        assert!(result.unwrap_err().contains("stopped unexpectedly"));
    }

    /// Whatever goes wrong once the row says `in_progress`, the row must end
    /// up saying something else, and the next refresh must be free to run.
    #[test]
    fn failed_fetch_leaves_the_generator_refreshable() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();

        let result = refresh_with_fetch(&fleet, node_id, |_| Err("timed out".into()));
        assert_eq!(result.unwrap_err(), "timed out");
        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.last_refresh_status.as_deref(), Some(REFRESH_ERROR));
        assert_eq!(config.last_refresh_error.as_deref(), Some("timed out"));

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Fetched", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();
        assert_eq!(managed_children(&fleet, list_id, node_id).len(), 1);

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    fn orphan_in_progress(fleet: &FleetStore, node_id: Uuid) {
        fleet
            .enqueue_outline(OutlineMutation::SetRefreshStatus {
                node_id,
                status: REFRESH_IN_PROGRESS.into(),
                error: None,
            })
            .unwrap();
        fleet.writer().flush().unwrap();
    }

    /// A process that exits mid-refresh leaves `in_progress` behind. That row
    /// must not turn every later refresh into a no-op.
    #[test]
    fn refresh_runs_despite_in_progress_left_by_a_dead_process() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node_id = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, node_id, DATA_SOURCE_MOCK, "{}").unwrap();
        orphan_in_progress(&fleet, node_id);

        let ds = MockDataSource::new().with_items(vec![item("EXT-1", "Fetched", vec![])]);
        refresh_generator_with(&fleet, node_id, &ds, &HashMap::new()).unwrap();

        assert_eq!(managed_children(&fleet, list_id, node_id).len(), 1);
        let config = read_config(&root, node_id).unwrap();
        assert_eq!(config.last_refresh_status.as_deref(), Some(REFRESH_SUCCESS));

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn launch_sweep_marks_orphaned_refreshes_failed() {
        let (root, fleet) = setup();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let orphaned = create_generator_node(&fleet, list_id);
        let settled = create_generator_node(&fleet, list_id);
        set_generator_config(&fleet, orphaned, DATA_SOURCE_MOCK, "{}").unwrap();
        set_generator_config(&fleet, settled, DATA_SOURCE_MOCK, "{}").unwrap();
        orphan_in_progress(&fleet, orphaned);

        assert_eq!(clear_interrupted_refreshes(&fleet).unwrap(), 1);

        let config = read_config(&root, orphaned).unwrap();
        assert_eq!(config.last_refresh_status.as_deref(), Some(REFRESH_ERROR));
        assert_eq!(
            config.last_refresh_error.as_deref(),
            Some(REFRESH_INTERRUPTED)
        );
        let config = read_config(&root, settled).unwrap();
        assert_eq!(config.last_refresh_status.as_deref(), Some(REFRESH_SUCCESS));
        assert_eq!(clear_interrupted_refreshes(&fleet).unwrap(), 0);

        drop(fleet);
        let _ = fs::remove_dir_all(root);
    }
}
