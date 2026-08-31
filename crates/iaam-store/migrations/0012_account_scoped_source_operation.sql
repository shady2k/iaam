-- Source operation identifiers are unique within an account when a channel declares
-- account-scoped identity. The new index is strictly weaker than the previous
-- (owner, source, source_operation_id) index: every old unique tuple remains unique
-- after account is added, so existing data is preserved without conflict.
DROP INDEX events_source_operation;

CREATE UNIQUE INDEX events_source_operation
    ON events (owner, source, account, source_operation_id)
    WHERE source_operation_id IS NOT NULL;
