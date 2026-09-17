create table orders (
    id       integer primary key autoincrement,
    customer text    not null,
    email    text    not null default '',
    amount   real    not null default 0,
    status   text    not null default 'new',
    created  text    not null default (datetime('now'))
);
