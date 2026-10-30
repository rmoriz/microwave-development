import { BASE_PATH } from '../data/config.ts';

const base = BASE_PATH.replace(/\/$/, '');

/** Prefixes an internal path with the base path; an already-based path is unchanged. */
export function withBase(path: string): string {
  if (/^[a-z][a-z0-9+.-]*:/i.test(path) || path.startsWith('//')) return path;
  const normalized = path.startsWith('/') ? path : `/${path}`;
  if (base && (normalized === base || normalized.startsWith(`${base}/`))) return normalized;
  return `${base}${normalized}`;
}
