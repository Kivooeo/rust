fn main() {
    let a = A;
    a.new();
    //~^ ERROR no method named `new` found for struct `A` in the current scope [E0599]
    let mut b = B;
    b.new();
    //~^ ERROR no method named `new` found for struct `B` in the current scope [E0599]
    let c = C;
    c.new();
    //~^ ERROR no method named `new` found for struct `C` in the current scope [E0599]
    let d = D;
    d.new();
    //~^ ERROR no method named `new` found for struct `D` in the current scope [E0599]
}
// Cases where methods was successfully found in any of &/& mut implementations
struct A;

trait X {
    fn new(&self) -> i32;
}

impl X for &A {
    fn new(&self) -> i32 {
        0
    }
}

struct B;

trait Y {
    fn new(&mut self) -> i32;
}

impl Y for &mut B {
    fn new(&mut self) -> i32 {
        0
    }
}

// Cases where methods was not successfully found in any of &/& mut implementations
struct C;

struct D;

trait Z {
    fn new(&self);
}
