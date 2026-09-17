// Той самий файл бази, що й у rhaix: порівнюємо рендер, а не драйвери.
// `node:sqlite` вбудований у Node 22 — жодних нативних збірок.
import { DatabaseSync } from 'node:sqlite';

const db = new DatabaseSync(process.env.ORDERS_DB ?? '../data/orders.db');

const SORTS = new Set(['id', 'customer', 'amount', 'created']);

export function findOrders({ status = '', sort = 'id', dir = 'desc', limit = 1000, skip = 0 }) {
  if (!SORTS.has(sort)) sort = 'id';
  if (dir !== 'asc') dir = 'desc';
  const where = status ? 'where status = ?' : '';
  const params = status ? [status, limit, skip] : [limit, skip];
  return db
    .prepare(`select * from orders ${where} order by ${sort} ${dir} limit ? offset ?`)
    .all(...params);
}

export function getOrder(id) {
  return db.prepare('select * from orders where id = ?').get(id);
}

export function countOrders() {
  return db.prepare('select count(*) as n from orders').get().n;
}

export function insertOrder({ customer, email, amount }) {
  const info = db
    .prepare('insert into orders (customer, email, amount, status) values (?, ?, ?, ?)')
    .run(customer, email, amount, 'new');
  return Number(info.lastInsertRowid);
}
