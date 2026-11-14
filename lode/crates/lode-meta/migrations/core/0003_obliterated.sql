-- Content obliterated from a depot (T120). The object is deleted from the blob store; the
-- row keeps it from being stored again and says who removed it, when and why. Trees and
-- revisions still name the id, so history stays intact.
CREATE TABLE obliterated (
    depot_id  INTEGER NOT NULL REFERENCES depot (id),
    id        BLOB    NOT NULL,
    size      INTEGER NOT NULL,
    path      TEXT    NOT NULL,
    reason    TEXT    NOT NULL,
    by_id     INTEGER NOT NULL REFERENCES principal (id),
    at        INTEGER NOT NULL,
    PRIMARY KEY (depot_id, id)
);
