// Human-readable model names for the launcher's model pickers (#687).
//
// RED-BEFORE-GREEN PLACEHOLDER — this commit deliberately ships TODAY'S behavior
// behind the new signature: the picker labels an option with its raw id and
// nothing else (`o.textContent = m`, launcher.ts). The next commit implements it.

export function prettyModelId(id: string): string {
  return id;
}

export function modelLabel(_cli: string, id: string): string {
  return id;
}
