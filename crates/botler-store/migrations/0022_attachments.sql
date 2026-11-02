-- Files a chat attached to its reply (`attach_files`, also from inside a script): its chat shows
-- them with the reply.
CREATE TABLE attachments (
    instance TEXT NOT NULL,
    at       TEXT NOT NULL,
    file     TEXT NOT NULL
) STRICT;
CREATE INDEX attachments_instance ON attachments (instance, at);
