-- Cat198x Schema Migration v2 -> v3
-- Adds is_disk to dat_roms so <disk> (CHD) entries are distinguished from <rom>
-- entries. A disk's sha1 is the CHD's internal logical-data hash (from the .chd
-- header), it has no size/crc, and it is always stored loose in a machine folder
-- rather than packed into an archive.

ALTER TABLE dat_roms ADD COLUMN is_disk INTEGER NOT NULL DEFAULT 0;
