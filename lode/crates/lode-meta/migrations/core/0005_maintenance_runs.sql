-- The last garbage collection and consistency check of each depot (T140), shown on the
-- admin web UI's overview. One row per depot and kind; a new run replaces the row.
CREATE TABLE maintenance_run (
    depot_id         INTEGER NOT NULL REFERENCES depot (id),
    kind             TEXT    NOT NULL,
    at               INTEGER NOT NULL,
    objects_checked  INTEGER NOT NULL,
    objects_affected INTEGER NOT NULL,
    bytes_affected   INTEGER NOT NULL,
    problems         INTEGER NOT NULL,
    PRIMARY KEY (depot_id, kind)
);
