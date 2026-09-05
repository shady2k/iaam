-- The standing classification rule that settled a fact, lifted out of the
-- payload so the journal can be narrowed by it.
--
-- The owner makes one decision, a rule is written from it, and the rule then
-- files a group of rows he never saw one by one. When one of them is wrong, the
-- way to find it is to see the group — and the group is defined by the rule.
-- Without this column that means reading a whole import by eye, which does not
-- scale past a page.
--
-- The version is a column of its own rather than folded into the identifier,
-- because the two are selected on separately: «the rows rule R filed» names no
-- version, and one column holding `rule/version` would have to be taken apart
-- to answer it. The version counts the owner's decisions rather than the rule's
-- own revisions — every rule he writes takes the next number in his sequence,
-- and an edit retires the rule and writes a new one under a new identifier —
-- so the pair recorded here is fixed at the moment the row is filed and stays
-- readable without consulting the rules, including for a rule since retired.
--
-- Filtering in Rust after the fact was the alternative and is not one: the
-- listing is paginated in SQL, so a filter applied afterwards would return short
-- pages and a cursor that skips rows. This is `0023_event_import_session.sql`'s
-- reasoning, one handle along.
--
-- Nullable, and NULL is the honest answer for three different rows: one
-- recorded before this column existed, one a reading settled without any rule
-- of his, and one written by a route that reads no rules at all — a correction,
-- a broker synchronisation. The column does not separate them and is not asked
-- to: it exists to select a *named* rule, and none of the three is one. The
-- separation the owner needs — «no rule settled this» against «nothing was
-- recorded about it» — lives on the fact itself, inside `payload`, where the
-- provenance carries a value for the first and nothing for the second.
--
-- Nothing is back-filled. The rule that settled an older row was never recorded
-- anywhere to back-fill it from, so an identifier written here would name a
-- decision the owner never made about that row.
ALTER TABLE events ADD COLUMN settled_by_rule TEXT;
ALTER TABLE events ADD COLUMN settled_by_rule_version INTEGER;

-- Partial, on the rows a rule filed. The rows without one are the majority and
-- are never what this index is asked about.
CREATE INDEX events_by_settled_by_rule
    ON events (owner, settled_by_rule)
    WHERE settled_by_rule IS NOT NULL;
