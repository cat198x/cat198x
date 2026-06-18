-- Schema v5: source disposition (consume vs preserve).
--
-- Replaces the per-invocation `plan --move` flag with a per-source property.
-- Existing sources migrate to 'preserve' — the safe default: nothing becomes
-- deletable without an explicit later choice. The genuine staging sources are
-- then flipped to 'consume' as a separate, visible step (not in this migration:
-- "not under a destination" does not mean "staging" — reference masters exist).
-- See decisions/source-disposition.md.
ALTER TABLE sources ADD COLUMN disposition TEXT NOT NULL DEFAULT 'preserve';
