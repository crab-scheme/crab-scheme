// Reference impl mirroring scheme/alloc-stress.scm. Boxed cons cells
// to match the Scheme heap allocation pattern.

// Cons holds a u64 payload to match the Scheme list element width
// (each pair carries an integer). The compiler doesn't see the field
// being read by name, only positionally in the while-let; suppress
// the dead-code lint.
#[allow(dead_code)]
enum List {
    Nil,
    Cons(u64, Box<List>),
}

fn make_list_n(n: u64) -> Box<List> {
    let mut acc = Box::new(List::Nil);
    for i in 0..n {
        acc = Box::new(List::Cons(i, acc));
    }
    acc
}

fn list_length(l: &List) -> u64 {
    let mut n = 0u64;
    let mut cur = l;
    while let List::Cons(_, rest) = cur {
        n += 1;
        cur = rest;
    }
    n
}

fn alloc_stress(rounds: u64) -> u64 {
    let mut sum = 0u64;
    for _ in 0..rounds {
        let l = make_list_n(1000);
        sum += list_length(&l);
    }
    sum
}

fn main() {
    let n = 200;
    println!("alloc-stress({}) = {}", n, alloc_stress(n));
}
