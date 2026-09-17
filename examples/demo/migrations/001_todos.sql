create table todos (
    id      integer primary key autoincrement,
    title   text    not null,
    done    integer not null default 0,
    created text    not null default (datetime('now'))
);

insert into todos (title, done) values
    ('Купити молоко', 1),
    ('Прочитати SYNTAX.md', 0);
