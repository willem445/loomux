// Fixture: the consumer. It imports every export that must NOT be reported dead,
// and imports nothing from `b.ts` — the cycle is closed by `a`/`b` themselves, not
// by this file.
import { usedByEntry } from './dead.ts';
import { alpha } from './a.ts';
import { longFunction, shortFunction } from './long.ts';

export function run(): number {
  return usedByEntry + alpha(1) + longFunction(2, 3) + shortFunction();
}
