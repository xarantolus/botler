-- What the user allowed a chat on top of its daily budget, for one day (the hub's local date).
CREATE TABLE budget_extra (
    instance TEXT NOT NULL,
    day      TEXT NOT NULL,
    usd      REAL NOT NULL DEFAULT 0,
    tokens   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (instance, day)
) STRICT;
