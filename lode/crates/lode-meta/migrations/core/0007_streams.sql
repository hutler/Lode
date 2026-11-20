-- Streams (SPEC §5.3a): a branch may be a `mainline`, `development` or `release` stream.
-- NULL keeps the v1 merge rules. The stream parent is the existing `parent_branch_id`.
ALTER TABLE branch ADD COLUMN stream_type TEXT;
