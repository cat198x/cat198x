-- ROMShelf Schema v1

-- Schema version tracking
CREATE TABLE schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Collections (e.g., "No-Intro", "TOSEC")
CREATE TABLE collections (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    source_type TEXT NOT NULL,  -- 'nointro', 'redump', 'tosec', 'mame', 'custom'
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Collection versions (one active per collection)
CREATE TABLE collection_versions (
    id INTEGER PRIMARY KEY,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    version TEXT NOT NULL,
    dat_path TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT 0,
    imported_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(collection_id, version)
);

-- DAT hierarchy nodes (for TOSEC: Manufacturer/System/Category)
CREATE TABLE dat_nodes (
    id INTEGER PRIMARY KEY,
    version_id INTEGER NOT NULL REFERENCES collection_versions(id) ON DELETE CASCADE,
    parent_id INTEGER REFERENCES dat_nodes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    node_type TEXT NOT NULL,  -- 'root', 'manufacturer', 'system', 'category', 'dat'
    path TEXT NOT NULL,       -- Full path like "TOSEC/Commodore/Amiga"
    UNIQUE(version_id, path)
);

-- Games/sets from DATs
CREATE TABLE dat_games (
    id INTEGER PRIMARY KEY,
    node_id INTEGER NOT NULL REFERENCES dat_nodes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    parent_name TEXT,         -- For clones
    is_bios BOOLEAN NOT NULL DEFAULT 0,
    is_device BOOLEAN NOT NULL DEFAULT 0,
    is_mechanical BOOLEAN NOT NULL DEFAULT 0,
    UNIQUE(node_id, name)
);

-- ROMs within games
CREATE TABLE dat_roms (
    id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES dat_games(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha1 TEXT,
    md5 TEXT,
    crc32 TEXT,
    status TEXT NOT NULL DEFAULT 'good',  -- 'good', 'baddump', 'nodump'
    merge_tag TEXT,           -- For merged sets
    UNIQUE(game_id, name)
);

-- Device references for MAME
CREATE TABLE dat_game_devices (
    id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES dat_games(id) ON DELETE CASCADE,
    device_name TEXT NOT NULL,
    UNIQUE(game_id, device_name)
);

-- Content-addressed file store. `sha1` is the full-file hash (the true bytes
-- on disk and the dedup identity); `sha1_no_header` is the headerless hash,
-- set only when a copier header was detected and stripped. Storing both lets a
-- file match either a headerless DAT (No-Intro) or a headered DAT (GoodTools).
CREATE TABLE files (
    sha1 TEXT PRIMARY KEY,
    sha1_no_header TEXT,
    md5 TEXT,
    crc32 TEXT,
    size INTEGER NOT NULL,
    first_seen TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Registered source directories
CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    case_sensitive BOOLEAN NOT NULL,
    added_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_scanned TEXT
);

-- Where files physically exist
CREATE TABLE file_locations (
    id INTEGER PRIMARY KEY,
    sha1 TEXT NOT NULL REFERENCES files(sha1),
    source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    path TEXT NOT NULL,                    -- Relative to source
    archive_path TEXT,                     -- Entry within archive, NULL if loose
    last_seen TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(source_id, path, archive_path)
);

-- Per-path configuration
CREATE TABLE dat_config (
    id INTEGER PRIMARY KEY,
    path_pattern TEXT NOT NULL UNIQUE,     -- Glob pattern
    dest_path TEXT,
    output_format TEXT,                    -- 'zip', 'torrentzip', 'loose'
    merge_mode TEXT,                       -- 'split', 'non-merged', 'merged'
    config_json TEXT                       -- Additional settings as JSON
);

-- Operation log for rollback
CREATE TABLE operation_log (
    id INTEGER PRIMARY KEY,
    plan_hash TEXT NOT NULL,
    operation_type TEXT NOT NULL,
    source_path TEXT,
    dest_path TEXT,
    reverse_operation TEXT,                -- JSON describing how to undo
    status TEXT NOT NULL DEFAULT 'pending',
    executed_at TEXT
);

-- Quarantine entries for files that were removed from destinations
CREATE TABLE quarantine (
    id INTEGER PRIMARY KEY,
    sha1 TEXT NOT NULL,
    original_path TEXT NOT NULL,           -- Where the file was before quarantine
    quarantine_path TEXT NOT NULL,         -- Path within .romshelf/quarantine/
    size INTEGER NOT NULL,
    reason TEXT NOT NULL,                  -- 'set_removed', 'content_changed', 'path_changed'
    collection_name TEXT,                  -- Which collection this was for (if known)
    quarantined_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(quarantine_path)
);

-- Indexes for common queries
CREATE INDEX idx_collection_versions_collection ON collection_versions(collection_id);
CREATE INDEX idx_collection_versions_active ON collection_versions(collection_id, is_active);
CREATE INDEX idx_dat_nodes_version ON dat_nodes(version_id);
CREATE INDEX idx_dat_nodes_parent ON dat_nodes(parent_id);
CREATE INDEX idx_dat_games_node ON dat_games(node_id);
CREATE INDEX idx_dat_games_parent ON dat_games(parent_name);
CREATE INDEX idx_dat_roms_game ON dat_roms(game_id);
CREATE INDEX idx_dat_roms_sha1 ON dat_roms(sha1);
CREATE INDEX idx_dat_roms_crc32 ON dat_roms(crc32);
CREATE INDEX idx_file_locations_sha1 ON file_locations(sha1);
CREATE INDEX idx_file_locations_source ON file_locations(source_id);
CREATE INDEX idx_files_sha1_no_header ON files(sha1_no_header);
CREATE INDEX idx_quarantine_sha1 ON quarantine(sha1);
CREATE INDEX idx_quarantine_collection ON quarantine(collection_name);
