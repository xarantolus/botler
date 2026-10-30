import type { Breaker } from "../generated";

/** The two agents of a pair breaker subject (`pair:<a>|<b>`), or null. */
export function pairOf(subject: string): [string, string] | null {
  if (!subject.startsWith("pair:")) return null;
  const [a, b, ...rest] = subject.slice(5).split("|");
  return a && b && rest.length === 0 ? [a, b] : null;
}

/** Tripped pair breakers holding messages to or from `instance`. */
export function heldFor(instance: string, breakers: Breaker[]): { breaker: Breaker; other: string }[] {
  const out: { breaker: Breaker; other: string }[] = [];
  for (const b of breakers) {
    const pair = pairOf(b.subject);
    if (!pair) continue;
    if (pair[0] === instance) out.push({ breaker: b, other: pair[1] });
    else if (pair[1] === instance) out.push({ breaker: b, other: pair[0] });
  }
  return out;
}

/** Breakers pausing `instance` itself (wake churn, budget, provider errors). */
export function pausing(instance: string, breakers: Breaker[]): Breaker[] {
  return breakers.filter((b) => b.subject === `instance:${instance}`);
}

export function isTripped(subject: string, breakers: Breaker[]): boolean {
  return breakers.some((b) => b.subject === subject);
}
