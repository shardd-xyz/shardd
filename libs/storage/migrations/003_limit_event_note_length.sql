ALTER TABLE events
    ADD CONSTRAINT chk_events_note_length
    CHECK (note IS NULL OR char_length(note) <= 4096) NOT VALID;
