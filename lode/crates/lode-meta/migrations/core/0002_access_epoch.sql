-- Server-wide counters. `access_epoch` is raised whenever an access rule or a group
-- membership changes (ARCHITECTURE §6, "Access epoch").
CREATE TABLE counter (
    name   TEXT    PRIMARY KEY,
    value  INTEGER NOT NULL
);

INSERT INTO counter (name, value) VALUES ('access_epoch', 1);
