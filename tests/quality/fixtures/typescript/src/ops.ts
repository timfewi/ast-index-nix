export function /*@def:zero*/zero(): number { return 0; }
export function /*@def:one*/one(value: number): number { return value + 1; }
export function /*@def:two*/two(a: number, b: number): number { return a + b; }
export function /*@def:flag*/flag(): boolean { return true; }
export function /*@def:identity*/identity<T>(value: T): T { return value; }
export function /*@def:pair*/pair<A, B>(a: A, b: B): [A, B] { return [a, b]; }
export function /*@def:select*/select<T>(a: T, _b: T): T { return a; }
export const /*@def:arrow*/arrow = (n: number): number => n + 2;
export default function /*@def:default_fn*/defaultFn(): number { return 3; }

export class /*@def:counter*/Counter {
  value = 0;
  /*@def:get*/get(): number { return this.value; }
  /*@def:add*/add(n: number): number { return this.value + n; }
  /*@def:put*/put(n: number): void { this.value = n; }
  static /*@def:static_zero*/zero(): number { return 0; }
  static /*@def:static_one*/one(n: number): number { return n + 1; }
  static /*@def:static_id*/identity<T>(n: T): T { return n; }
}
export class /*@def:other*/Other {
  /*@def:other_get*/get(): number { return 7; }
}
