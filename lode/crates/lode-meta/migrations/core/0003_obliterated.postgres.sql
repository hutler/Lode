-- Postgres form of 0003_obliterated.sql (T153): BIGINT for every integer, BYTEA for blobs, identity
-- columns for row ids, and the C collation so text sorts by bytes as in SQLite.
-- Content obliterated from a depot (T120). The object is deleted from the blob store; the
-- row keeps it from being stored again and says who removed it, when and why. Trees and
-- revisions still name the id, so history stays intact.
CREATE TABLE obliterated (
    depot_id  BIGINT NOT NULL REFERENCES depot (id),
    id        BYTEA   NOT NULL,
    size      BIGINT NOT NULL,
    path      TEXT COLLATE "C" NOT NULL,
    reason    TEXT COLLATE "C" NOT NULL,
    by_id     BIGINT NOT NULL REFERENCES principal (id),
    at        BIGINT NOT NULL,
    PRIMARY KEY (depot_id, id)
);
