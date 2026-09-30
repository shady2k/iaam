-- The instance's stored broker-egress switch, and the record of its first
-- enabling (iaam-h0b8i.1.2).
--
-- Broker egress used to be a process configuration only: `serve` sent broker
-- requests when the environment variable `IAAM_BROKER_EGRESS` was `on` at
-- start. The owner had to remember a variable for every restart, and a
-- restart without it silently turned his broker synchronisations off. This
-- table is his word instead: a successful `iaam broker connect` turns the
-- switch on, `iaam broker off` turns it off, and `serve` reads it at start.
-- The environment variable survives as an override — `off` still forces the
-- switch off for a deployment that must not call a broker, and `on` stays
-- accepted for developer tools — but the stored value is the default.
--
-- **One row, the whole statement.** A fresh instance has no row: no broker
-- has been connected, so broker egress is off. The primary key pins the
-- single `setting` atom, the way a singleton table must: a second row would
-- be two answers to one question, and the CHECK refuses any other name.
--
-- **`first_enabled_at` is the tally's voucher, not a timestamp of curiosity.**
-- A missing or empty tally record is the conservative recovery state: iaam
-- records 1,000 attempts for every broker endpoint, so the endpoint is at its
-- daily ceiling for 24 hours. That stops a lost record from restoring an
-- allowance, but it would also stop the owner's first `connect` from working
-- on the instance it just created. So the first enabling (by `connect`)
-- creates a fresh zero tally beside the database before its check call goes
-- out, and records the moment here. From then on this column is set, and a
-- deleted or emptied egress directory is the conservative state exactly as
-- before: deleting the tally restores nothing, because the database vouches
-- only for the tally it watched being minted. The column is never cleared:
-- turning the switch off does not un-happen the first enabling.
--
-- **Why this table does not travel in a bundle.** It describes this instance
-- on this machine — its tally, its egress — not the portfolio. A restore that
-- carried it would turn broker requests on in the target instance without the
-- owner having connected anything there. See
-- `iaam_store::bundle::TableDisposition` for the classification.
CREATE TABLE broker_egress (
    setting          TEXT NOT NULL PRIMARY KEY CHECK (setting = 'broker egress'),
    enabled          INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    first_enabled_at TEXT,
    recorded_at      TEXT NOT NULL
) STRICT;
