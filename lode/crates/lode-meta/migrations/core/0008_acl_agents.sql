-- Access rules for every agent at once (T194): subject kind `agents`.
--
-- SQLite cannot change a CHECK constraint in place, so the table is rebuilt with the same
-- columns and ids. Nothing references `acl` and it has no indexes of its own.
CREATE TABLE acl_new (
    id            INTEGER PRIMARY KEY,
    depot_id      INTEGER REFERENCES depot (id),
    subject_kind  TEXT NOT NULL CHECK (subject_kind IN ('all', 'user', 'group', 'agents')),
    subject_id    INTEGER,
    branch_glob   TEXT NOT NULL,
    path_glob     TEXT NOT NULL,
    effect        TEXT NOT NULL CHECK (effect IN ('allow', 'deny')),
    level         TEXT NOT NULL CHECK (level IN ('read', 'write', 'admin'))
);
INSERT INTO acl_new (id, depot_id, subject_kind, subject_id, branch_glob, path_glob, effect, level)
    SELECT id, depot_id, subject_kind, subject_id, branch_glob, path_glob, effect, level FROM acl;
DROP TABLE acl;
ALTER TABLE acl_new RENAME TO acl;
