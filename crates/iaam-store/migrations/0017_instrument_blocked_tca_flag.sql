ALTER TABLE instruments ADD COLUMN blocked_tca_flag INTEGER NOT NULL DEFAULT 0
    CHECK (blocked_tca_flag IN (0, 1));
