//! Performance baseline: native Rust vs tree-walker vs bytecode VM.
//!
//! All tests are `#[ignore]`; run with:
//!   `cargo test --release --test perf_baseline -- --ignored --nocapture`
//!
//! For each workload we print walker ms / VM ms / native Rust ms and the
//! "slowdown vs native" ratios so we know the speed-of-light ceiling and
//! how much headroom the interpreters have left.

use cs_runtime::Runtime;

/// Average runtime over `iters` runs (after one warm-up). Returns ms.
fn bench<F: FnMut()>(iters: u32, mut f: F) -> f64 {
    f(); // warm-up
    let t = std::time::Instant::now();
    for _ in 0..iters {
        f();
    }
    let total = t.elapsed().as_secs_f64() * 1000.0;
    total / iters as f64
}

/// For very fast native workloads, run `inner` iterations *inside* the
/// timed closure so the per-call cost rises above measurement noise (~ns
/// resolution). Returns ms per single call.
fn bench_batched<F: FnMut()>(iters: u32, inner: u32, mut f: F) -> f64 {
    let inner_ms = bench(iters, || {
        for _ in 0..inner {
            f();
        }
    });
    inner_ms / inner as f64
}

fn fmt_time(ms: f64) -> String {
    if ms >= 1.0 {
        format!("{:7.2}ms", ms)
    } else if ms >= 0.001 {
        format!("{:7.2}µs", ms * 1000.0)
    } else {
        format!("{:7.2}ns", ms * 1_000_000.0)
    }
}

fn print_row(label: &str, native: f64, walker: f64, vm: f64) {
    let walker_x = if native > 0.0 {
        walker / native
    } else {
        f64::INFINITY
    };
    let vm_x = if native > 0.0 {
        vm / native
    } else {
        f64::INFINITY
    };
    let vm_vs_walker = if vm > 0.0 { walker / vm } else { 0.0 };
    println!(
        "{:30}  native={}  walker={} ({:7.0}x)  vm={} ({:7.0}x)  vm/walker={:.2}x",
        label,
        fmt_time(native),
        fmt_time(walker),
        walker_x,
        fmt_time(vm),
        vm_x,
        vm_vs_walker
    );
}

// ---- workload 1: recursive fib(25) ----

#[inline(never)]
fn native_fib(n: i64) -> i64 {
    if n < 2 {
        n
    } else {
        native_fib(n - 1) + native_fib(n - 2)
    }
}

#[test]
#[ignore]
fn perf_fib_25() {
    let src =
        "(letrec ((fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))) (fib 25))";
    let iters = 5;
    // fib(25) is ~250k function calls; native runs in ~30µs, plenty above noise.
    let native = bench(iters, || {
        let n = std::hint::black_box(25);
        let r = native_fib(n);
        std::hint::black_box(r);
    });
    let walker = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str("perf-walker", src).unwrap();
    });
    let vm = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("perf-vm", src).unwrap();
    });
    print_row("fib(25) recursive", native, walker, vm);
}

// ---- workload 2: tail-recursive iter loop, 1,000,000 iterations ----

#[inline(never)]
fn native_loop(n: i64) -> i64 {
    let mut acc: i64 = 0;
    let mut i = n;
    while i > 0 {
        acc += 1;
        i -= 1;
    }
    acc
}

#[test]
#[ignore]
fn perf_tail_loop_100k() {
    // 100k iterations: stays under the walker's 1M depth limit (its TCE
    // doesn't stop depth from growing on tail calls in foundation eval).
    let src = "(letrec ((loop (lambda (n acc) \
               (if (= n 0) acc (loop (- n 1) (+ acc 1)))))) \
               (loop 100000 0))";
    let iters = 5;
    let native = bench(iters, || {
        let n = std::hint::black_box(100_000);
        let r = native_loop(n);
        std::hint::black_box(r);
    });
    let walker = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str("perf-walker", src).unwrap();
    });
    let vm = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("perf-vm", src).unwrap();
    });
    print_row("loop 100k (tail-call)", native, walker, vm);
}

// ---- workload 3: factorial(20) tail-iter ----

#[inline(never)]
fn native_fact_iter(n: i64) -> i64 {
    let mut acc: i64 = 1;
    let mut i = n;
    while i > 0 {
        acc *= i;
        i -= 1;
    }
    acc
}

#[test]
#[ignore]
fn perf_fact_iter_20() {
    let src = "(letrec ((f (lambda (n acc) (if (= n 0) acc (f (- n 1) (* n acc)))))) (f 20 1))";
    let iters = 5;
    // fact(20) is ~20 mul ops — too fast to time directly; batch 10k inner.
    let native = bench_batched(iters, 10_000, || {
        let n = std::hint::black_box(20);
        std::hint::black_box(native_fact_iter(n));
    });
    let walker = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str("perf-walker", src).unwrap();
    });
    let vm = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("perf-vm", src).unwrap();
    });
    print_row("fact(20) tail-iter", native, walker, vm);
}

// ---- workload 4: sum 1..=10000 via fold-left ----

#[inline(never)]
fn native_sum_to(n: i64) -> i64 {
    // Hand-rolled loop to mirror what the walker does (10000 actual adds)
    // rather than letting LLVM close-form to n*(n+1)/2.
    let mut acc: i64 = 0;
    let mut i: i64 = 1;
    while i <= n {
        acc = std::hint::black_box(acc + i);
        i += 1;
    }
    acc
}

#[test]
#[ignore]
fn perf_fold_sum_10k() {
    let src = "(define (range-up to) \
               (letrec ((go (lambda (i acc) \
               (if (> i to) (reverse acc) (go (+ i 1) (cons i acc)))))) \
               (go 1 '()))) \
               (fold-left + 0 (range-up 10000))";
    let iters = 5;
    // 10000 adds is ~3µs in native; batch 1000 inner.
    let native = bench_batched(iters, 1_000, || {
        let n = std::hint::black_box(10_000);
        std::hint::black_box(native_sum_to(n));
    });
    let walker = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str("perf-walker", src).unwrap();
    });
    let vm = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("perf-vm", src).unwrap();
    });
    print_row("fold-left + 0 (range 10k)", native, walker, vm);
}

// ---- workload 5: map (lambda (x) (* x x)) over 1000 ints ----

#[inline(never)]
fn native_map_squares(xs: &[i64]) -> Vec<i64> {
    xs.iter().map(|x| x * x).collect()
}

#[test]
#[ignore]
fn perf_map_squares_1k() {
    let src = "(define (range-up to) \
               (letrec ((go (lambda (i acc) \
               (if (> i to) (reverse acc) (go (+ i 1) (cons i acc)))))) \
               (go 1 '()))) \
               (length (map (lambda (x) (* x x)) (range-up 1000)))";
    let iters = 5;
    let native_xs: Vec<i64> = (1..=1000).collect();
    // Each map+collect is ~5µs; batch 200 inner.
    let native = bench_batched(iters, 200, || {
        let xs = std::hint::black_box(&native_xs);
        std::hint::black_box(native_map_squares(xs));
    });
    let walker = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str("perf-walker", src).unwrap();
    });
    let vm = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("perf-vm", src).unwrap();
    });
    print_row("map x*x (range 1k)", native, walker, vm);
}

// ---- workload 6: tree-recursive ackermann(3, 6) ----

#[inline(never)]
fn native_ack(m: i64, n: i64) -> i64 {
    if m == 0 {
        n + 1
    } else if n == 0 {
        native_ack(m - 1, 1)
    } else {
        native_ack(m - 1, native_ack(m, n - 1))
    }
}

#[test]
#[ignore]
fn perf_ackermann_3_6() {
    let src = "(letrec ((ack (lambda (m n) \
               (if (= m 0) (+ n 1) \
                   (if (= n 0) (ack (- m 1) 1) \
                       (ack (- m 1) (ack m (- n 1)))))))) \
               (ack 3 6))";
    let iters = 5;
    // ack(3,6) is ~170k recursive calls; native runs in ~1ms.
    let native = bench(iters, || {
        let m = std::hint::black_box(3);
        let n = std::hint::black_box(6);
        std::hint::black_box(native_ack(m, n));
    });
    let walker = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str("perf-walker", src).unwrap();
    });
    let vm = bench(iters, || {
        let mut rt = Runtime::new();
        rt.eval_str_via_vm("perf-vm", src).unwrap();
    });
    print_row("ackermann(3,6)", native, walker, vm);
}

// ---- summary header (run any single test to see) ----

#[test]
#[ignore]
fn perf_baseline_header() {
    println!();
    println!(
        "{:30}  {:^20}  {:^25}  {:^25}  {}",
        "workload", "native (Rust)", "tree-walker (slowdown)", "vm (slowdown)", "vm/walker"
    );
    println!("{}", "-".repeat(120));
}
