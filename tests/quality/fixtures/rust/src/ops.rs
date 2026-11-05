pub fn /*@def:zero*/zero() -> u32 { 0 }
pub fn /*@def:one*/one(value: u32) -> u32 { value + 1 }
pub fn /*@def:two*/two(a: u32, b: u32) -> u32 { a + b }
pub fn /*@def:identity*/identity<T>(value: T) -> T { value }
pub fn /*@def:pair*/pair<A, B>(a: A, b: B) -> (A, B) { (a, b) }
pub fn /*@def:select*/select<T>(a: T, _b: T) -> T { a }
pub fn /*@def:flag*/flag() -> bool { true }

pub struct Counter { pub value: u32 }
impl Counter {
    pub fn /*@def:new*/new(value: u32) -> Self { Self { value } }
    pub fn /*@def:get*/get(&self) -> u32 { self.value }
    pub fn /*@def:add*/add(&self, value: u32) -> u32 { self.value + value }
    pub fn /*@def:put*/put(&mut self, value: u32) { self.value = value; }
    pub fn /*@def:take*/take(self) -> u32 { self.value }
}

pub struct /*@def:holder*/Holder(pub Counter);
pub struct Other;
impl Other {
    pub fn /*@def:other_get*/get(&self) -> u32 { 7 }
}

pub trait Dynamic { fn get(&self) -> u32; }
