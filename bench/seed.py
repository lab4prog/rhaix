# -*- coding: utf-8 -*-
"""Наповнити спільну базу для порівняння: обидва застосунки читають цей файл."""
import os
import random
import sqlite3

HERE = os.path.dirname(os.path.abspath(__file__))
PATH = os.path.join(HERE, 'data', 'orders.db')

os.makedirs(os.path.dirname(PATH), exist_ok=True)
if os.path.exists(PATH):
    os.remove(PATH)

db = sqlite3.connect(PATH)
db.execute("""create table orders (
    id       integer primary key autoincrement,
    customer text    not null,
    email    text    not null default '',
    amount   real    not null default 0,
    status   text    not null default 'new',
    created  text    not null default (datetime('now'))
)""")

# Таблиця міграцій rhaix: щоб він не намагався застосувати свою поверх готової.
db.execute("""create table _rhaix_migrations (
    name text primary key,
    applied_at text not null default (datetime('now'))
)""")
db.execute("insert into _rhaix_migrations (name) values ('001_orders.sql')")

random.seed(7)
first = ['Оксана', 'Петро', 'Марія', 'Ігор', 'Олена',
         'Богдан', 'Наталія', 'Андрій', 'Юлія', 'Тарас']
last = ['Литвин', 'Гнатюк', 'Шевчук', 'Ткаченко', 'Коваль',
        'Бондар', 'Мельник', 'Кравець', 'Поліщук', 'Савченко']
statuses = ['new', 'paid', 'cancelled']

rows = []
for i in range(1000):
    rows.append((
        f"{random.choice(first)} {random.choice(last)}",
        f"user{i}@example.com",
        round(random.uniform(50, 20000), 2),
        random.choice(statuses),
        f"2026-0{random.randint(1, 9)}-{random.randint(10, 28)} "
        f"1{random.randint(0, 9)}:0{random.randint(0, 9)}:00",
    ))
db.executemany(
    "insert into orders (customer, email, amount, status, created) values (?,?,?,?,?)",
    rows,
)
db.commit()
print('записів:', db.execute("select count(*) from orders").fetchone()[0])
db.close()
