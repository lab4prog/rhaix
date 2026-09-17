create table orders (
    id       integer primary key autoincrement,
    customer text    not null,
    email    text    not null default '',
    amount   real    not null default 0,
    status   text    not null default 'new',
    created  text    not null default (datetime('now'))
);

insert into orders (customer, email, amount, status) values
    ('Оксана Литвин',  'o@example.com', 1240.0,  'paid'),
    ('Петро Гнатюк',   'p@example.com',  380.5,  'new'),
    ('Марія Шевчук',   'm@example.com', 15600.0, 'paid'),
    ('Ігор Ткаченко',  'i@example.com',   99.99, 'cancelled');
