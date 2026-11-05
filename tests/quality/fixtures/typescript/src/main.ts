import defaultFn, { zero, one, two, flag, identity, pair, select, arrow, Counter, Other } from "./ops";
import { zero as aliasZero, one as aliasOne, two as aliasTwo } from "./ops";
import * as ops from "./ops";
import * as facade from "./facade";
import { increment, defaultFn as exportedDefault } from "./facade";

export function local() {
  const a = /*@call:local.01=>zero*/zero();
  const b = /*@call:local.02=>one*/one(a);
  const c = /*@call:local.03=>two*/two(a, b);
  if (/*@call:local.04=>flag*/flag()) { /*@call:local.05=>one*/one(c); }
  const tuple = [/*@call:local.06=>zero*/zero(), /*@call:local.07=>one*/one(2)];
  /*@call:local.08=>arrow*/arrow(4);
  /*@call:local.09=>default_fn*/defaultFn();
  return /*@call:local.10=>two*/two(tuple[0], tuple[1]);
}

export function qualified() {
  ops./*@call:qualified.01=>zero*/zero();
  ops./*@call:qualified.02=>one*/one(1);
  ops./*@call:qualified.03=>two*/two(1, 2);
  ops./*@call:qualified.04=>flag*/flag();
  ops./*@call:qualified.05=>arrow*/arrow(1);
  /*@call:qualified.06=>zero*/aliasZero();
  /*@call:qualified.07=>one*/aliasOne(1);
  /*@call:qualified.08=>two*/aliasTwo(1, 2);
  ops./*@call:qualified.09=>default_fn*/default();
  ops./*@call:qualified.10=>identity*/identity("synthetic");
}

export function reexports() {
  facade./*@call:reexport.01=>zero*/zero();
  facade./*@call:reexport.02=>one*/increment(1);
  facade./*@call:reexport.03=>two*/two(1, 2);
  facade./*@call:reexport.04=>flag*/flag();
  /*@call:reexport.05=>one*/increment(2);
  facade./*@call:reexport.06=>default_fn*/defaultFn();
  /*@call:reexport.07=>default_fn*/exportedDefault();
  facade./*@call:reexport.08=>identity*/identity(false);
  facade.Counter./*@call:reexport.09=>static_one*/one(3);
  facade.Counter./*@call:reexport.10=>static_zero*/zero();
}

export function generics() {
  /*@call:generic.01=>identity*/identity<number>(1);
  /*@call:generic.02=>identity*/identity<string>("value");
  /*@call:generic.03=>identity*/identity<boolean>(false);
  /*@call:generic.04=>pair*/pair<number, string>(1, "two");
  /*@call:generic.05=>pair*/pair<boolean, number>(true, 3);
  /*@call:generic.06=>select*/select<number>(1, 2);
  ops./*@call:generic.07=>identity*/identity<[number, boolean]>([1, false]);
  ops./*@call:generic.08=>pair*/pair(3, 4);
  ops./*@call:generic.09=>select*/select(false, true);
  facade./*@call:generic.10=>identity*/identity<"text">("text");
}

// Fresh, non-escaping concrete objects. A typed parameter is a negative case below.
export function receivers() {
  const value = new /*@call:constructor.01=>counter*/Counter();
  value./*@call:receiver.01=>put*/put(1);
  value./*@call:receiver.02=>get*/get();
  value./*@call:receiver.03=>add*/add(2);
  const other = new /*@call:constructor.02=>other*/Other();
  other./*@call:receiver.04=>other_get*/get();
  (new /*@call:constructor.03=>counter*/Counter())./*@call:receiver.05=>get*/get();
  (new /*@call:constructor.04=>counter*/Counter())./*@call:receiver.06=>add*/add(3);
  (new /*@call:constructor.05=>other*/Other())./*@call:receiver.07=>other_get*/get();
  const second = new /*@call:constructor.06=>counter*/Counter();
  second./*@call:receiver.08=>put*/put(4);
  second./*@call:receiver.09=>get*/get();
  second./*@call:receiver.10=>add*/add(5);
  new ops./*@call:constructor.07=>counter*/Counter();
  new facade./*@call:constructor.08=>counter*/Counter();
  new ops./*@call:constructor.09=>other*/Other();
  new /*@call:constructor.10=>counter*/Counter();
}

export function associated() {
  Counter./*@call:associated.01=>static_zero*/zero();
  Counter./*@call:associated.02=>static_one*/one(1);
  Counter./*@call:associated.03=>static_id*/identity(2);
  ops.Counter./*@call:associated.04=>static_zero*/zero();
  ops.Counter./*@call:associated.05=>static_one*/one(3);
  ops.Counter./*@call:associated.06=>static_id*/identity<number>(4);
  facade.Counter./*@call:associated.07=>static_zero*/zero();
  facade.Counter./*@call:associated.08=>static_one*/one(5);
  facade.Counter./*@call:associated.09=>static_id*/identity<boolean>(true);
  Counter./*@call:associated.10=>static_id*/identity<string>("x");
}

export function nested() {
  /*@call:nested.01=>one*/one(/*@call:nested.02=>zero*/zero());
  /*@call:nested.03=>two*/two(/*@call:nested.04=>zero*/zero(), /*@call:nested.05=>one*/one(1));
  /*@call:nested.06=>one*/one(
    /*@call:nested.07=>two*/two(1, /*@call:nested.08=>zero*/zero())
  );
  /*@call:nested.09=>one*/one(/*@call:nested.10=>one*/one(4));
}

export function scopes() {
  function /*@def:local_zero*/zero() { return 42; }
  /*@call:scope.01=>local_zero*/zero();
  ops./*@call:scope.02=>zero*/zero();
  {
    const /*@def:block_one*/one = (_n: number) => 1;
    /*@call:scope.03=>block_one*/one(2);
    ops./*@call:scope.04=>one*/one(3);
  }
  /*@call:scope.05=>one*/one(4);
  {
    const /*@def:block_expr*/zero = function(a: number, b: number) { return a + b; };
    /*@call:scope.06=>block_expr*/zero(1, 2);
    ops./*@call:scope.07=>flag*/flag();
  }
  /*@call:scope.08=>local_zero*/zero();
  ops./*@call:scope.09=>one*/one(5);
  /*@call:scope.10=>two*/two(1, 2);
}

// Fixed control family before producer/overlay implementation.
export function heldout() {
  const result = /*@call:heldout.01=>flag*/flag()
    ? /*@call:heldout.02=>one*/one(1) : /*@call:heldout.03=>two*/two(2, 3);
  let n = 0;
  while (n < /*@call:heldout.04=>one*/one(2)) { n += /*@call:heldout.05=>one*/one(0); }
  const condition = /*@call:heldout.06=>flag*/flag() && /*@call:heldout.07=>flag*/flag();
  return condition ? /*@call:heldout.08=>one*/one(result)
    : /*@call:heldout.09=>two*/two(result, /*@call:heldout.10=>zero*/zero());
}

export function uncertain(callback: () => number, receiver: Counter, structural: {get(): number}) {
  /*@deny:safety.callback*/callback();
  receiver./*@deny:safety.overridable*/get();
  structural./*@deny:safety.structural*/get();
}
