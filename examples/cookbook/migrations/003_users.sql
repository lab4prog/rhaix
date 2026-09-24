-- Користувачі з ролями — для рецепта «ролі й права» (middleware.rhx).
-- Пароля тут немає навмисно: рецепт про права, а не про вхід. Справжній вхід
-- із паролем і лімітом спроб — examples/demo/pages/login.rhx.
create table users (
    id    integer primary key autoincrement,
    name  text    not null,
    role  text    not null default 'viewer'
);

insert into users (name, role) values
    ('Адміністраторка Ірина', 'admin'),
    ('Менеджер Андрій',       'manager'),
    ('Переглядач Олег',       'viewer');
