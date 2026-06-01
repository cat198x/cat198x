-- Cat198x Schema Migration v1 -> v2
-- Adds quarantine table for managing removed/displaced files

-- Quarantine entries for files that were removed from destinations
CREATE TABLE quarantine (
    id INTEGER PRIMARY KEY,
    sha1 TEXT NOT NULL,
    original_path TEXT NOT NULL,           -- Where the file was before quarantine
    quarantine_path TEXT NOT NULL,         -- Path within .cat198x/quarantine/
    size INTEGER NOT NULL,
    reason TEXT NOT NULL,                  -- 'set_removed', 'content_changed', 'path_changed'
    collection_name TEXT,                  -- Which collection this was for (if known)
    quarantined_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(quarantine_path)
);

CREATE INDEX idx_quarantine_sha1 ON quarantine(sha1);
CREATE INDEX idx_quarantine_collection ON quarantine(collection_name);
