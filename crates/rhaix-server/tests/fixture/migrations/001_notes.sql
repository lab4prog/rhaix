create table notes (
    id    integer primary key autoincrement,
    title text    not null,
    done  integer not null default 0
);

insert into notes (title, done) values ('перша', 0), ('друга', 1);
