//! DDL for outline schema (v3+).

/// SQL to create all outline tables (idempotent `IF NOT EXISTS`).
pub const OUTLINE_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS lists (
    id          BLOB PRIMARY KEY NOT NULL,
    slug        TEXT NOT NULL UNIQUE CHECK (length(slug) <= 40),
    title       TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_lists_slug_folded ON lists(lower(slug));

CREATE TABLE IF NOT EXISTS nodes (
    id              BLOB PRIMARY KEY NOT NULL,
    slug            TEXT NOT NULL UNIQUE CHECK (length(slug) <= 40),
    title           TEXT NOT NULL,
    kind            TEXT NOT NULL DEFAULT 'normal'
                    CHECK (kind IN ('normal', 'reference')),
    ref_target_id   BLOB REFERENCES nodes(id) ON DELETE RESTRICT,
    slug_manual     INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    CHECK (
        (kind = 'reference' AND ref_target_id IS NOT NULL)
        OR (kind = 'normal' AND ref_target_id IS NULL)
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_nodes_slug_folded ON nodes(lower(slug));
CREATE INDEX IF NOT EXISTS idx_nodes_ref_target ON nodes(ref_target_id);

CREATE TABLE IF NOT EXISTS node_capabilities (
    node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    capability  TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent')),
    enabled_at  INTEGER NOT NULL,
    PRIMARY KEY (node_id, capability)
);

CREATE TABLE IF NOT EXISTS capability_archives (
    id              BLOB PRIMARY KEY NOT NULL,
    node_id         BLOB NOT NULL,
    capability      TEXT NOT NULL CHECK (capability IN ('spec', 'lifecycle', 'agent')),
    archived_at     INTEGER NOT NULL,
    payload         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_capability_archives_node ON capability_archives(node_id, archived_at);

CREATE TABLE IF NOT EXISTS node_lifecycle (
    node_id     BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    state       TEXT NOT NULL CHECK (state IN (
                    'proposed', 'design', 'planning', 'ready', 'active',
                    'verifying', 'review', 'approved', 'merged',
                    'released', 'learn', 'done'
                )),
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS outline_entries (
    node_id     BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    list_id     BLOB NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    parent_id   BLOB REFERENCES nodes(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL DEFAULT 0,
    collapsed   INTEGER NOT NULL DEFAULT 0,
    CHECK (parent_id IS NULL OR parent_id != node_id)
);
CREATE INDEX IF NOT EXISTS idx_outline_list_parent ON outline_entries(list_id, parent_id, ordinal);

CREATE TABLE IF NOT EXISTS node_obligations (
    id              BLOB PRIMARY KEY NOT NULL,
    node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('requirement', 'constraint')),
    ordinal         INTEGER NOT NULL,
    section         TEXT,
    body            TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    UNIQUE (node_id, kind, ordinal)
);
CREATE INDEX IF NOT EXISTS idx_obligations_node ON node_obligations(node_id, kind, ordinal);

CREATE TABLE IF NOT EXISTS global_obligations (
    id          BLOB PRIMARY KEY NOT NULL,
    slug        TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('requirement', 'constraint')),
    ordinal     INTEGER NOT NULL,
    body        TEXT NOT NULL,
    adopted     INTEGER NOT NULL DEFAULT 1,
    UNIQUE (slug, kind, ordinal)
);

CREATE TABLE IF NOT EXISTS node_extra_content (
    id           BLOB PRIMARY KEY NOT NULL,
    node_id      BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL CHECK (content_type IN ('goal', 'design', 'plan', 'notes')),
    body         TEXT NOT NULL DEFAULT '',
    updated_at   INTEGER NOT NULL,
    UNIQUE (node_id, content_type)
);

CREATE TABLE IF NOT EXISTS interview_transcripts (
    id              BLOB PRIMARY KEY NOT NULL,
    node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    phase           TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    body            TEXT NOT NULL DEFAULT '',
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_transcripts_node ON interview_transcripts(node_id, created_at);

CREATE TABLE IF NOT EXISTS node_fields (
    node_id         BLOB PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    repo            TEXT,
    branch          TEXT,
    notes           TEXT,
    tags            TEXT NOT NULL DEFAULT '[]',
    linked_issues   TEXT NOT NULL DEFAULT '[]',
    linked_prs      TEXT NOT NULL DEFAULT '[]',
    updated_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS media_assets (
    id              BLOB PRIMARY KEY NOT NULL,
    relative_path   TEXT NOT NULL UNIQUE,
    content_type    TEXT,
    byte_size       INTEGER NOT NULL,
    sha256          TEXT NOT NULL,
    created_at      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS node_media_links (
    node_id     BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    media_id    BLOB NOT NULL REFERENCES media_assets(id) ON DELETE RESTRICT,
    role        TEXT NOT NULL,
    label       TEXT,
    ordinal     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, media_id, role)
);

CREATE TABLE IF NOT EXISTS list_health_issues (
    id          BLOB PRIMARY KEY NOT NULL,
    list_id     BLOB NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    issue_type  TEXT NOT NULL CHECK (issue_type IN ('reference_loop')),
    detail      TEXT NOT NULL,
    detected_at INTEGER NOT NULL,
    cleared_at  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_list_health_open ON list_health_issues(list_id) WHERE cleared_at IS NULL;

CREATE TABLE IF NOT EXISTS interview_sessions (
    id              BLOB PRIMARY KEY NOT NULL,
    node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    agent_config_id TEXT REFERENCES agent_configs(id),
    display_name    TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('active', 'archived', 'complete')),
    phase           TEXT NOT NULL,
    session_id      TEXT,
    scratchpad_path TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_interview_sessions_node ON interview_sessions(node_id, status);

CREATE TABLE IF NOT EXISTS gate_criteria (
    id          BLOB PRIMARY KEY NOT NULL,
    from_state  TEXT NOT NULL,
    to_state    TEXT NOT NULL,
    slug        TEXT NOT NULL UNIQUE,
    label       TEXT NOT NULL,
    sort_order  INTEGER NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_gate_criteria_transition
    ON gate_criteria(from_state, to_state, sort_order);

CREATE TABLE IF NOT EXISTS node_gate_evaluations (
    node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    criterion_id    BLOB NOT NULL REFERENCES gate_criteria(id) ON DELETE CASCADE,
    outcome         TEXT NOT NULL CHECK (outcome IN ('pass', 'fail', 'pending', 'waived')),
    detail          TEXT,
    source          TEXT NOT NULL CHECK (source IN ('agent', 'human', 'derived')),
    evaluated_at    INTEGER NOT NULL,
    PRIMARY KEY (node_id, criterion_id)
);
CREATE INDEX IF NOT EXISTS idx_node_gate_evaluations_node
    ON node_gate_evaluations(node_id, evaluated_at);

CREATE TABLE IF NOT EXISTS _legacy_task_node_map (
    legacy_task_id TEXT PRIMARY KEY NOT NULL,
    node_id        BLOB NOT NULL UNIQUE REFERENCES nodes(id) ON DELETE CASCADE
);
"#;
