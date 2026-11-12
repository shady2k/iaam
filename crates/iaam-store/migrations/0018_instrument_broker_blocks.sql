-- The old aggregate was last-write-wins and has no recoverable handle evidence.
-- Broker description refreshes rebuild it without inventing a legacy handle.
CREATE TABLE instrument_broker_blocks (
    instrument TEXT NOT NULL REFERENCES instruments(id),
    handle TEXT NOT NULL CHECK (length(handle) > 0),
    blocked_tca_flag INTEGER NOT NULL CHECK (blocked_tca_flag IN (0, 1)),
    PRIMARY KEY (instrument, handle)
) STRICT;
UPDATE instruments SET blocked_tca_flag = 0;
