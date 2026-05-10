// Reference impl mirroring scheme/fib.scm.
// Build: rustc -O fib.rs -o fib
// Run:   ./fib

fn fib(n: u64) -> u64 {
    if n < 2 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn main() {
    let n = 25;
    println!("fib({}) = {}", n, fib(n));
}
