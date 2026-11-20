-- Postgres form of 0007_acl_agents.sql (T194): subject kind `agents` matches every agent.
ALTER TABLE acl DROP CONSTRAINT IF EXISTS acl_subject_kind_check;
ALTER TABLE acl ADD CONSTRAINT acl_subject_kind_check
    CHECK (subject_kind IN ('all', 'user', 'group', 'agents'));
