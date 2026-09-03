// Fixture source for the clippy parser. Clippy spans carry no function NAME, so
// the parser reads it back out of the source at the span start; these two `fn`
// lines are what it reads. Never compiled — no crate manifest points at it.
pub fn big_fn(a: u8, b: u8, c: u8) -> u8 {
    a + b + c
}

// A doc comment and an attribute sit between the span start and the `fn` line, so
// this pins that the name scan looks past them rather than only at line_start.
/// Doc.
#[inline]
pub fn attributed_fn() -> u8 {
    1
}
