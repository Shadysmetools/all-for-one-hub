// Words for "which session does the default chat use, and why". Rust decides the target
// (route.rs); this only says it, in the board's language.
import type { SessionRoute } from './ipc';
import type { Strings } from './team/i18n';

export function routeText(r: SessionRoute, l: Strings): string {
  const t = r.tab_title;
  const base = r.via === 'pin' ? l.routePin(t) : r.via === 'coordinator' ? l.routeCoordinator(t) : r.via === 'all_for_one' ? l.routeAfo(t) : l.routeNone;
  return r.pin_fell_back ? `${base} ${l.routePinGone}` : base;
}
