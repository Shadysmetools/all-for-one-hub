// A tiny, safe markdown renderer for One's replies. It builds DOM nodes with textContent only:
// model text is NEVER put into innerHTML. Supported: paragraphs, headings (shown bold), bullet
// and numbered lists, **bold**, *italic*, `inline code`, fenced code blocks (with a Copy
// button) and [text](link). A link is only live when it is http(s); anything else shows as text.

/** The address when it is a plain http(s) URL, else null. */
export function safeLink(raw: string): string | null {
  try {
    const u = new URL(raw);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null;
  } catch {
    return null;
  }
}

type OpenLink = (url: string) => void;

/** Options: `linksOff` shows links as plain text (another session's text may be in play). */
export interface MdOptions {
  linksOff?: boolean;
}

/** Replies longer than this are drawn as markdown up to here; the rest sits behind "Show more" as plain text. */
export const MAX_RENDER = 20000;

// Every quantifier is bounded, so a hostile run of brackets cannot make a render slow.
const INLINE = /(`[^`\n]{1,500}`)|(\*\*[^*\n]{1,500}\*\*)|(\*[^*\s][^*\n]{0,500}\*)|(\[[^\[\]\n]{1,300}\]\([^)\s]{1,2000}\))/g;

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  return e;
}

function inline(parent: HTMLElement, text: string, open: OpenLink, opts: MdOptions): void {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    const at = m.index ?? 0;
    if (at > last) parent.append(text.slice(last, at));
    const tok = m[0];
    if (m[1]) {
      const c = el('code', 'md-code');
      c.textContent = tok.slice(1, -1);
      parent.append(c);
    } else if (m[2]) {
      const b = el('strong');
      inline(b, tok.slice(2, -2), open, opts);
      parent.append(b);
    } else if (m[3]) {
      const i = el('em');
      inline(i, tok.slice(1, -1), open, opts);
      parent.append(i);
    } else {
      const close = tok.indexOf('](');
      parent.append(link(tok.slice(1, close), tok.slice(close + 2, -1), open, opts));
    }
    last = at + tok.length;
  }
  if (last < text.length) parent.append(text.slice(last));
}

function link(label: string, url: string, open: OpenLink, opts: MdOptions): Node {
  const safe = safeLink(url);
  if (!safe || opts.linksOff) return document.createTextNode(label); // no click without a safe address or in a locked conversation
  const a = el('a', 'md-link');
  a.textContent = label;
  a.href = safe;
  a.rel = 'noopener noreferrer';
  a.title = safe;
  a.onclick = (e) => {
    e.preventDefault(); // never navigate the app window; the system browser opens it (after a confirm)
    open(safe);
  };
  a.onauxclick = (e) => e.preventDefault();
  a.oncontextmenu = (e) => e.preventDefault();
  // the real destination is always visible next to the label
  const host = document.createElement('span');
  host.className = 'md-host';
  host.textContent = ` (${new URL(safe).hostname})`;
  const frag = document.createDocumentFragment();
  frag.append(a, host);
  return frag;
}

function codeBlock(code: string): HTMLElement {
  const box = el('div', 'md-block');
  const pre = el('pre');
  const c = el('code');
  c.textContent = code;
  pre.append(c);
  const copy = el('button', 'md-copy');
  copy.type = 'button';
  copy.textContent = 'Copy';
  copy.onclick = () => {
    void navigator.clipboard.writeText(code).then(
      () => (copy.textContent = 'Copied'),
      () => (copy.textContent = 'Copy failed'),
    );
    setTimeout(() => (copy.textContent = 'Copy'), 1500);
  };
  box.append(copy, pre);
  return box;
}

/** Replace the children of `into` with the rendered `src`. Safe to call again on every streamed delta. */
export function renderMarkdown(into: HTMLElement, src: string, open: OpenLink, opts: MdOptions = {}): void {
  into.replaceChildren();
  if (src.length > MAX_RENDER) {
    renderBlocks(into, src.slice(0, MAX_RENDER), open, opts);
    const rest = src.slice(MAX_RENDER);
    const more = el('button', 'md-more');
    more.type = 'button';
    more.textContent = `Show more (${rest.length} more characters)`;
    more.onclick = () => {
      const p = el('p');
      p.textContent = rest; // the rest is plain text: no markdown pass over a huge reply
      more.replaceWith(p);
    };
    into.append(more);
    return;
  }
  renderBlocks(into, src, open, opts);
}

function renderBlocks(into: HTMLElement, src: string, open: OpenLink, opts: MdOptions): void {
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  let para: string[] = [];
  let list: HTMLElement | null = null;
  const flush = () => {
    if (!para.length) return;
    const p = el('p');
    inline(p, para.join(' '), open, opts);
    into.append(p);
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*```/.test(line)) {
      flush();
      list = null;
      const code: string[] = [];
      while (++i < lines.length && !/^\s*```/.test(lines[i])) code.push(lines[i]);
      into.append(codeBlock(code.join('\n'))); // an unfinished block (still streaming) renders as it is
      continue;
    }
    const h = line.match(/^\s{0,3}#{1,6}\s+(.*)$/);
    const li = line.match(/^\s*([-*+]|\d+[.)])\s+(.*)$/);
    if (h) {
      flush();
      list = null;
      const p = el('p', 'md-h');
      const b = el('strong');
      inline(b, h[1], open, opts);
      p.append(b);
      into.append(p);
    } else if (li) {
      flush();
      const tag = /\d/.test(li[1]) ? 'ol' : 'ul';
      if (!list || list.tagName.toLowerCase() !== tag) {
        list = el(tag);
        into.append(list);
      }
      const item = el('li');
      inline(item, li[2], open, opts);
      list.append(item);
    } else if (!line.trim()) {
      flush();
      list = null;
    } else {
      list = null;
      para.push(line.trim());
    }
  }
  flush();
}
