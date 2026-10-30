import { BASE_PATH } from '../data/config';

const base = BASE_PATH.replace(/\/$/, '');

export function withBase(path: string): string {
  return `${base}${path.startsWith('/') ? path : `/${path}`}`;
}
