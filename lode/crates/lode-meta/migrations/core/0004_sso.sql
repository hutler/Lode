-- Single sign-on (T122): the identity a provider names for a user, such as their e-mail
-- address, stored in lower case. At most one principal per identity.
ALTER TABLE principal ADD COLUMN sso_id TEXT;
CREATE UNIQUE INDEX principal_sso_id ON principal (sso_id);
