create table users (
    id       integer primary key autoincrement,
    username text    not null unique,
    password text    not null
);
