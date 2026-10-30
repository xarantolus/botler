// Stands in for `@iconify/vue` (see the alias in vite.config.ts) so the app never fetches icons.
//
// Nuxt UI imports `Icon`, `iconLoaded` and `addIcon` from `@iconify/vue`. The online build loads
// any icon it does not have from api.iconify.design; the hub may run without internet, so this
// shim uses the offline build instead, which only renders icons registered at build time (Nuxt UI
// bundles every icon the sources name, `icon.clientBundle.scan`).
//
// The offline storage looks names up verbatim, while Nuxt UI renders `i-lucide-x` as `lucide-x`
// and registers `lucide:x`: each icon is stored under both spellings.

import { addIcon as addOffline, Icon, type IconifyIcon } from "@iconify/vue/offline";

const loaded = new Set<string>();

export function addIcon(name: string, data: IconifyIcon): boolean {
  const names = [name];
  const colon = name.indexOf(":");
  if (colon > 0) names.push(`${name.slice(0, colon)}-${name.slice(colon + 1)}`);
  for (const n of names) {
    addOffline(n, data);
    loaded.add(n);
  }
  return true;
}

export function iconLoaded(name: string): boolean {
  return loaded.has(name);
}

export { Icon };
