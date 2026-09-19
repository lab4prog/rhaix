-- Токени для `api/`. Зберігається хеш, а не сам токен: витік таблиці не має
-- одразу давати доступ, так само як із паролями.
create table api_tokens (
    id      integer primary key autoincrement,
    label   text    not null,
    hash    text    not null unique,
    created text    not null default (datetime('now'))
);

-- Демонстраційний токен: `demo-token-42`. Реальний робиться `rhaix` -скриптом
-- і показується власнику один раз.
insert into api_tokens (label, hash) values
    ('демо', '5461e4e32374937dda117fb92165ed134aed9c16b8f5cecf1807fc42d87ffda2');
