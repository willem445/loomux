// Fixture: the other half of the cycle. The edge back to `a` is what makes this
// an SCC rather than two independent nodes.
import { alpha } from './a.ts';

export function beta(n: number): number {
  return n > 100 ? n : alpha(n - 1);
}
