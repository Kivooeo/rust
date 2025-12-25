//! Regression test for <https://github.com/rust-lang/rust/issues/150354>
//!                     <https://github.com/rust-lang/rust/issues/150355>
//!
//@ edition 2024

#![allow(incomplete_features)]
#![feature(min_generic_const_args, adt_const_params, generic_const_items, generic_const_exprs)]

enum Option<T> {
    Some(T),
    None,
}

fn foo<const N: Option<u32>>() {} //~ ERROR `Option<u32>` must implement `ConstParamTy` to be used as the type of a const generic parameter [E0741]

fn bar<T>() {
    foo::<{ Some::<u32> { 0: const {} } }>();
    //~^ ERROR mismatched types [E0308]
    //~| ERROR the constant `std::option::Option::<u32>::Some({})` is not of type `Option<u32>`
}

fn baz<T, const N: u32>() {
    foo::<{ Some::<u32> { 0: N } }>;
    //~^ ERROR the constant `std::option::Option::<u32>::Some(N)` is not of type `Option<u32>`
}

fn main() {}
