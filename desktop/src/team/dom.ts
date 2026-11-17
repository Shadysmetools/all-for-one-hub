// Tiny DOM builder. Text is always added as text nodes (textContent semantics),
// never as HTML, so board data cannot inject markup.
type Prop = string | number | boolean | undefined | ((e: Event) => void);
export type Kid = Node | string | number | null | false | undefined;

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Record<string, Prop> | null,
  ...kids: Kid[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props ?? {})) {
    const aria = k.startsWith('aria-');
    if (v === undefined || (v === false && !aria)) continue;
    if (typeof v === 'function') el.addEventListener(k.replace(/^on/, ''), v);
    else if (k === 'class') el.className = String(v);
    else el.setAttribute(k, v === true && !aria ? '' : String(v)); // aria-* booleans are "true"/"false"
  }
  for (const kid of kids) {
    if (kid === null || kid === false || kid === undefined) continue;
    el.append(typeof kid === 'string' || typeof kid === 'number' ? document.createTextNode(String(kid)) : kid);
  }
  return el;
}
