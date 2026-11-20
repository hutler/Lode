-- Postgres form of 0004_sso.sql (T153): BIGINT for every integer, BYTEA for blobs, identity
-- columns for row ids, and the C collation so text sorts by bytes as in SQLite.
-- Single sign-on (T122): the identity a provider names for a user, such as their e-mail
-- address, stored in lower case. At most one principal per identity.
ALTER TABLE principal ADD COLUMN sso_id TEXT COLLATE "C";
CREATE UNIQUE INDEX principal_sso_id ON principal (sso_id);
