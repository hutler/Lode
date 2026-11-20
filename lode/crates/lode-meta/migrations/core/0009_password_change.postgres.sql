-- Postgres form of 0007_password_change.sql: BIGINT for every integer.
--
-- A password that must be changed before it can be used for anything else, such as the one
-- `loded setup --generate-admin-password` prints. The value is the time (ms) after which it
-- stops working altogether; NULL for an ordinary password.
ALTER TABLE principal ADD COLUMN password_change_by BIGINT;
