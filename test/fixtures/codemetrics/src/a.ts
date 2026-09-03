// Fixture: half of the two-module import cycle. `a` imports `b` and `b` imports
// `a`, so the SCC census must report exactly one component of exactly these two.
import { beta } from './b.ts';

export function alpha(n: number): number {
  return beta(n) + 1;
}
