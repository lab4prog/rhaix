// Аналог scripts/orders.rhai — спільні функції проєкту.
export function statusLabel(status) {
  return { new: 'Новий', paid: 'Оплачено', cancelled: 'Скасовано' }[status] ?? status;
}

export function statusClass(status) {
  return `badge badge-${status}`;
}

export function money(value, decimals = 2) {
  const fixed = Math.abs(value).toFixed(decimals);
  const [whole, fraction] = fixed.split('.');
  const grouped = whole.replace(/\B(?=(\d{3})+(?!\d))/g, ' ');
  const sign = value < 0 ? '-' : '';
  return fraction ? `${sign}${grouped},${fraction}` : `${sign}${grouped}`;
}

export function date(value, pattern = 'DD.MM.YYYY HH:mm') {
  const d = new Date(String(value).replace(' ', 'T') + 'Z');
  const pad = (n) => String(n).padStart(2, '0');
  return pattern
    .replace('YYYY', d.getUTCFullYear())
    .replace('MM', pad(d.getUTCMonth() + 1))
    .replace('DD', pad(d.getUTCDate()))
    .replace('HH', pad(d.getUTCHours()))
    .replace('mm', pad(d.getUTCMinutes()));
}

export function url(path, params) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null && value !== '') search.set(key, value);
  }
  const query = search.toString();
  return query ? `${path}?${query}` : path;
}
