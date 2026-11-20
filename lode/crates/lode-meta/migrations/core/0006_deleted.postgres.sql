-- Postgres form of 0006_deleted.sql (T153): BIGINT for every integer.
--
-- A deleted principal keeps its row, because revisions name their author and submitter by
-- id: history keeps showing the name, and the name stays taken. `deleted_at` hides it from
-- lists and lookups by name.
ALTER TABLE principal ADD COLUMN deleted_at BIGINT;

-- A deleted depot goes to the trash first (`deleted_at`), keeping its name so it can be
-- restored. Purging erases its history and content and frees the name; the row stays as a
-- tombstone (`purged_at`, renamed) so the id is never given to another depot, whose objects
-- and module rows are keyed by it.
ALTER TABLE depot ADD COLUMN deleted_at BIGINT;
ALTER TABLE depot ADD COLUMN purged_at BIGINT;
