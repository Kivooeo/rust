#![allow(
    dead_code,
    unused_must_use
)]

struct Named {
    foo: usize
}

struct Unnamed(usize);

fn named_struct_field_access(named: &Named) {
    named->foo; //~ ERROR `->` is not valid for field access or method call
}

fn unnamed_struct_field_access(unnamed: &Unnamed) {
    unnamed->0; //~ ERROR `->` is not valid for field access or method call
}

fn tuple_field_access(t: &(u8, u8)) {
    t->0; //~ ERROR `->` is not valid for field access or method call
    t->1; //~ ERROR `->` is not valid for field access or method call
}

#[derive(Clone)]
struct Foo;

fn method_call(foo: &Foo) {
    foo->clone(); //~ ERROR `->` is not valid for field access or method call
}

fn main() {}
