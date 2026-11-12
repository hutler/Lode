-- Core schema, ARCHITECTURE §4.2. Times are milliseconds since the Unix epoch, UTC.

CREATE TABLE depot (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    next_rev    INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE principal (
    id             INTEGER PRIMARY KEY,
    kind           TEXT    NOT NULL CHECK (kind IN ('user', 'agent')),
    name           TEXT    NOT NULL UNIQUE,
    display_name   TEXT,
    owner_id       INTEGER REFERENCES principal (id),
    password_hash  TEXT,
    disabled       INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL
);

CREATE TABLE grp (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE
);

CREATE TABLE grp_member (
    grp_id        INTEGER NOT NULL REFERENCES grp (id) ON DELETE CASCADE,
    principal_id  INTEGER NOT NULL REFERENCES principal (id) ON DELETE CASCADE,
    PRIMARY KEY (grp_id, principal_id)
);

CREATE TABLE token (
    id            INTEGER PRIMARY KEY,
    principal_id  INTEGER NOT NULL REFERENCES principal (id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    hash          BLOB    NOT NULL UNIQUE,
    scopes        TEXT    NOT NULL,
    expires_at    INTEGER,
    last_used_at  INTEGER,
    created_at    INTEGER NOT NULL
);

CREATE TABLE acl (
    id            INTEGER PRIMARY KEY,
    depot_id      INTEGER REFERENCES depot (id),
    subject_kind  TEXT NOT NULL CHECK (subject_kind IN ('all', 'user', 'group')),
    subject_id    INTEGER,
    branch_glob   TEXT NOT NULL,
    path_glob     TEXT NOT NULL,
    effect        TEXT NOT NULL CHECK (effect IN ('allow', 'deny')),
    level         TEXT NOT NULL CHECK (level IN ('read', 'write', 'admin'))
);

CREATE TABLE branch (
    id                INTEGER PRIMARY KEY,
    depot_id          INTEGER NOT NULL REFERENCES depot (id),
    name              TEXT    NOT NULL,
    head_rev          INTEGER NOT NULL,
    created_rev       INTEGER NOT NULL,
    parent_branch_id  INTEGER REFERENCES branch (id),
    parent_rev        INTEGER,
    state             TEXT    NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'archived')),
    UNIQUE (depot_id, name)
);

CREATE TABLE revision (
    depot_id      INTEGER NOT NULL REFERENCES depot (id),
    num           INTEGER NOT NULL,
    branch_id     INTEGER NOT NULL REFERENCES branch (id),
    parent_num    INTEGER,
    root_tree     BLOB    NOT NULL,
    author_id     INTEGER NOT NULL REFERENCES principal (id),
    submitter_id  INTEGER NOT NULL REFERENCES principal (id),
    created_at    INTEGER NOT NULL,
    message       TEXT    NOT NULL,
    provenance    TEXT,
    request_id    TEXT,
    PRIMARY KEY (depot_id, num),
    UNIQUE (depot_id, request_id)
);

CREATE TABLE rev_change (
    depot_id        INTEGER NOT NULL,
    rev             INTEGER NOT NULL,
    path            TEXT    NOT NULL,
    action          TEXT    NOT NULL CHECK (action IN ('A', 'M', 'D')),
    kind            INTEGER,
    old_id          BLOB,
    new_id          BLOB,
    size            INTEGER,
    flags           INTEGER NOT NULL DEFAULT 0,
    copy_from_path  TEXT,
    copy_from_rev   INTEGER,
    PRIMARY KEY (depot_id, rev, path),
    FOREIGN KEY (depot_id, rev) REFERENCES revision (depot_id, num)
);

CREATE INDEX rev_change_path ON rev_change (depot_id, path, rev);

CREATE TABLE rev_merge (
    depot_id          INTEGER NOT NULL,
    rev               INTEGER NOT NULL,
    source_branch_id  INTEGER NOT NULL REFERENCES branch (id),
    from_rev          INTEGER NOT NULL,
    to_rev            INTEGER NOT NULL,
    FOREIGN KEY (depot_id, rev) REFERENCES revision (depot_id, num)
);

CREATE TABLE rev_meta (
    depot_id  INTEGER NOT NULL,
    rev       INTEGER NOT NULL,
    key       TEXT    NOT NULL,
    value     TEXT    NOT NULL,
    PRIMARY KEY (depot_id, rev, key),
    FOREIGN KEY (depot_id, rev) REFERENCES revision (depot_id, num)
);

CREATE TABLE outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    depot_id    INTEGER,
    kind        TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE outbox_cursor (
    subscriber  TEXT    PRIMARY KEY,
    last_id     INTEGER NOT NULL
);
