-- Cat198x Schema Migration v3 -> v4
-- Adds a covering index on files(crc32, size).
--
-- The match query's CRC32+size branch (find_matched_roms) is the *only* branch
-- that fires for CRC-only DATs — notably the merged arcade sets (MAME,
-- FinalBurn Neo), whose <rom> entries carry a CRC but no SHA1. Without this
-- index that join full-scans the whole files table once per ROM; on a
-- ~1.4M-file library that turns a single collection's plan into hundreds of
-- billions of row comparisons (the FinalBurn Neo - Arcade Games hang). With it,
-- the branch uses an index exactly like the SHA1 branches the query was built
-- around. IF NOT EXISTS so it is a no-op where it was already created by hand.

CREATE INDEX IF NOT EXISTS idx_files_crc32_size ON files(crc32, size);
