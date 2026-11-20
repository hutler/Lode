-- Postgres form of 0002_access_epoch.sql (T153): BIGINT for every integer, BYTEA for blobs, identity
-- columns for row ids, and the C collation so text sorts by bytes as in SQLite.
-- Server-wide counters. `access_epoch` is raised whenever an access rule or a group
-- membership changes (ARCHITECTURE §6, "Access epoch").
CREATE TABLE counter (
    name   TEXT COLLATE "C" PRIMARY KEY,
    value  BIGINT NOT NULL
);

INSERT INTO counter (name, value) VALUES ('access_epoch', 1);
