// Fixture: the dead-export discriminator. Two exports, identical in every way
// except that `usedByEntry` has an importer and `usedByNobody` has none. A
// dead-export check that reports both, or neither, fails here — which a fixture
// carrying only the dead one could not detect (#1182).
export const usedByNobody = 41;
export const usedByEntry = 42;
