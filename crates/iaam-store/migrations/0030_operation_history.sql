-- The forward direction of a correction chain, and the rule an owner's
-- answer minted for the row it addressed.
--
-- `relation` points only backwards: a replacement names the event it
-- replaces, a reversal names the event it undoes, but nothing on the
-- original names what came after it. Assembling one operation's history —
-- what it was when it arrived, every change since, what it is now — means
-- repeatedly asking "which fact names this one as its target", and without
-- an index that question is a full scan of the journal. Partial, on the rows
-- that actually name a target, because a standalone fact is the majority
-- and is never what this index is asked about — exactly
-- `events_by_settled_by_rule`'s reasoning (`0029`), one handle along.
CREATE INDEX events_by_relation_target
    ON events (owner, relation_target)
    WHERE relation_target IS NOT NULL;

-- The classification rule an owner's answer minted, recorded on the
-- observation it answered rather than on the sibling question.
--
-- `resolution_of` reads the owner's answer off the observation, not off the
-- question: that is where the minted rule has to live for the fact committed
-- from it to be able to say "this row was settled by the very answer that
-- also became a rule", instead of reading like a row no rule ever touched.
--
-- Attaching it to `import_questions.rule` instead — where the identifier a
-- rule mints is already recorded — would change what the assessment says
-- about every sibling question in the group, and the owner would be told,
-- once per row of the group, that a rule was written from his one answer.
--
-- The version is a column of its own, matching `events.settled_by_rule` and
-- `events.settled_by_rule_version` (`0029`): a rule can be edited after it is
-- minted, and "the rule this answer minted" and "the rule at whatever
-- version it holds now" are two different questions. Folding both into one
-- column would answer only the second.
--
-- Both columns are nullable, and nothing is back-filled. NULL is the honest
-- answer for every observation recorded before this migration and for every
-- observation whose answer mints no rule at all — an agent's answer that may
-- not generalise, or an answer whose shape does not generalise. Neither case
-- is a decision this software could reconstruct after the fact.
ALTER TABLE import_observations ADD COLUMN answer_rule TEXT;
ALTER TABLE import_observations ADD COLUMN answer_rule_version INTEGER;
