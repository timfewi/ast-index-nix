#![no_std]
#![allow(dead_code, unused_variables, unused_imports)]

mod ops;
use ops::{zero, one, two, flag, Counter, Holder, Other};
use crate::ops::{zero as alias_zero, one as alias_one, two as alias_two};
use crate::ops as module_alias;
pub mod facade { pub use crate::ops::{zero, one, two, flag, Counter}; }
pub use crate::ops::zero as public_zero;

fn local() {
    let a = /*@call:local.01=>zero*/zero();
    let b = /*@call:local.02=>one*/one(a);
    let c = /*@call:local.03=>two*/two(a, b);
    if /*@call:local.04=>flag*/flag() { let d = /*@call:local.05=>one*/one(c); }
    let tuple = (/*@call:local.06=>zero*/zero(), /*@call:local.07=>one*/one(2));
    let array = [/*@call:local.08=>two*/two(1, 2), /*@call:local.09=>one*/one(3)];
    let _ = { /*@call:local.10=>zero*/zero() };
}

fn qualified() {
    crate::ops::/*@call:qualified.01=>zero*/zero();
    crate::ops::/*@call:qualified.02=>one*/one(1);
    ops::/*@call:qualified.03=>two*/two(1, 2);
    self::ops::/*@call:qualified.04=>flag*/flag();
    module_alias::/*@call:qualified.05=>zero*/zero();
    module_alias::/*@call:qualified.06=>one*/one(2);
    /*@call:qualified.07=>zero*/alias_zero();
    /*@call:qualified.08=>one*/alias_one(2);
    /*@call:qualified.09=>two*/alias_two(2, 3);
    module_alias::/*@call:qualified.10=>two*/two(1, 3);
}

fn reexports() {
    facade::/*@call:reexport.01=>zero*/zero();
    facade::/*@call:reexport.02=>one*/one(1);
    facade::/*@call:reexport.03=>two*/two(1, 2);
    facade::/*@call:reexport.04=>flag*/flag();
    /*@call:reexport.05=>zero*/public_zero();
    crate::/*@call:reexport.06=>zero*/public_zero();
    facade::Counter::/*@call:reexport.07=>new*/new(0);
    use facade::{one as renamed, two as combine};
    /*@call:reexport.08=>one*/renamed(2);
    /*@call:reexport.09=>two*/combine(2, 3);
    self::facade::/*@call:reexport.10=>zero*/zero();
}

fn generics() {
    ops::/*@call:generic.01=>identity*/identity::<u8>(1);
    ops::/*@call:generic.02=>identity*/identity::<u32>(2);
    ops::/*@call:generic.03=>identity*/identity::<bool>(true);
    ops::/*@call:generic.04=>pair*/pair::<u8, u32>(1, 2);
    ops::/*@call:generic.05=>pair*/pair::<bool, u8>(true, 3);
    ops::/*@call:generic.06=>select*/select::<u32>(1, 2);
    ops::/*@call:generic.07=>identity*/identity((1_u8, false));
    ops::/*@call:generic.08=>pair*/pair(3_u32, 4_u8);
    ops::/*@call:generic.09=>select*/select(false, true);
    ops::/*@call:generic.10=>identity*/identity::<&str>("synthetic");
}

fn receivers(value: &Counter, mutable: &mut Counter, other: &Other, holder: &Holder) {
    value./*@call:receiver.01=>get*/get();
    value./*@call:receiver.02=>add*/add(1);
    mutable./*@call:receiver.03=>put*/put(3);
    mutable./*@call:receiver.04=>get*/get();
    other./*@call:receiver.05=>other_get*/get();
    holder.0./*@call:receiver.06=>get*/get();
    holder.0./*@call:receiver.07=>add*/add(2);
    (&value)./*@call:receiver.08=>get*/get();
    (*value)./*@call:receiver.09=>add*/add(4);
    (&mut *mutable)./*@call:receiver.10=>put*/put(6);
}

fn constructed() {
    let mut value = Counter { value: 1 };
    value./*@call:constructed.01=>put*/put(2);
    value./*@call:constructed.02=>add*/add(3);
    value./*@call:constructed.03=>get*/get();
    let borrowed = &value;
    borrowed./*@call:constructed.04=>get*/get();
    borrowed./*@call:constructed.05=>add*/add(5);
    let other = Other;
    other./*@call:constructed.06=>other_get*/get();
    (Counter { value: 7 })./*@call:constructed.07=>take*/take();
    (Counter { value: 8 })./*@call:constructed.08=>get*/get();
    (/*@call:constructed.11=>holder*/Holder(Counter { value: 9 })).0./*@call:constructed.09=>add*/add(1);
    value./*@call:constructed.10=>take*/take();
}

fn associated(value: &Counter) {
    Counter::/*@call:associated.01=>new*/new(1);
    Counter::/*@call:associated.02=>get*/get(value);
    Counter::/*@call:associated.03=>add*/add(value, 2);
    crate::ops::Counter::/*@call:associated.04=>new*/new(3);
    <Counter>::/*@call:associated.05=>new*/new(4);
    <Counter>::/*@call:associated.06=>get*/get(value);
    ops::Counter::/*@call:associated.07=>add*/add(value, 5);
    module_alias::Counter::/*@call:associated.08=>new*/new(6);
    self::ops::Counter::/*@call:associated.09=>get*/get(value);
    facade::Counter::/*@call:associated.10=>add*/add(value, 7);
}

fn nested() {
    /*@call:nested.01=>one*/one(/*@call:nested.02=>zero*/zero());
    /*@call:nested.03=>two*/two(/*@call:nested.04=>zero*/zero(), /*@call:nested.05=>one*/one(1));
    /*@call:nested.06=>one*/one(
        /*@call:nested.07=>two*/two(1, /*@call:nested.08=>zero*/zero())
    );
    /*@call:nested.09=>one*/one(/*@call:nested.10=>one*/one(4));
}

fn scopes() {
    fn /*@def:local_zero*/zero() -> u32 { 42 }
    /*@call:scope.01=>local_zero*/zero();
    crate::ops::/*@call:scope.02=>zero*/zero();
    {
        fn /*@def:block_one*/one(_: u32) -> u32 { 1 }
        /*@call:scope.03=>block_one*/one(2);
        crate::ops::/*@call:scope.04=>one*/one(3);
    }
    /*@call:scope.05=>one*/one(4);
    {
        use crate::ops::two as zero;
        /*@call:scope.06=>two*/zero(1, 2);
        super_alias::/*@call:scope.07=>flag*/flag();
    }
    /*@call:scope.08=>local_zero*/zero();
    module_alias::/*@call:scope.09=>one*/one(5);
    /*@call:scope.10=>two*/two(1, 2);
}
use crate::ops as super_alias;

// Held-out control-flow family: fixed before semantic adapters are implemented.
fn heldout_control(value: &Counter) -> u32 {
    let result = match /*@call:heldout.01=>zero*/zero() {
        0 => /*@call:heldout.02=>one*/one(1),
        _ => /*@call:heldout.03=>two*/two(2, 3),
    };
    let mut n = 0;
    while n < /*@call:heldout.04=>one*/one(2) { n += /*@call:heldout.05=>one*/one(0); }
    let condition = /*@call:heldout.06=>flag*/flag() && /*@call:heldout.07=>flag*/flag();
    if condition { value./*@call:heldout.08=>add*/add(result) }
    else { /*@call:heldout.09=>two*/two(result, value./*@call:heldout.10=>get*/get()) }
}

fn uncertain(callback: fn() -> u32, receiver: &dyn ops::Dynamic) {
    /*@deny:safety.callback*/callback();
    receiver./*@deny:safety.dynamic_dispatch*/get();
}
