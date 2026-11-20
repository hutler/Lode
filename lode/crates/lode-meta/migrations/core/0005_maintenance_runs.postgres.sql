-- Postgres form of 0005_maintenance_runs.sql (T153): BIGINT for every integer, BYTEA for blobs, identity
-- columns for row ids, and the C collation so text sorts by bytes as in SQLite.
-- The last garbage collection and consistency check of each depot (T140), shown on the
-- admin web UI's overview. One row per depot and kind; a new run replaces the row.
CREATE TABLE maintenance_run (
    depot_id         BIGINT NOT NULL REFERENCES depot (id),
    kind             TEXT COLLATE "C" NOT NULL,
    at               BIGINT NOT NULL,
    objects_checked  BIGINT NOT NULL,
    objects_affected BIGINT NOT NULL,
    bytes_affected   BIGINT NOT NULL,
    problems         BIGINT NOT NULL,
    PRIMARY KEY (depot_id, kind)
);
