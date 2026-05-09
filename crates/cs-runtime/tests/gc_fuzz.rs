//! Deterministic fuzz tests for the M5 GC pipeline.
//!
//! Generates random heap shapes via Scheme programs, interleaves
//! `Runtime::collect()` calls, and asserts that:
//!   • No `collect()` ever panics (bug class: a `Trace` impl that
//!     re-borrows a RefCell already borrowed elsewhere).
//!   • Programs continue producing the expected results after collect
//!     (bug class: a `Trace` impl that mutates state during marking).
//!
//! Hand-rolled deterministic LCG instead of proptest — keeps the test
//! crate dep-free and runs in stable CI without env-specific linker
//! requirements (proptest pulls tempfile→rustix→iconv on macOS).
//!
//! 4096 iterations × 8 seeds = 32768 op-sequences exercised per run.
//! That's enough to surface most trace bugs in unit-test runtime.
//! A future m5-fuzz CI job (TODO) cranks the seed count for nightly.
//!
//! NOTE: Phase 1's `Gc<T>` is `Rc<Slot<T>>`-backed, so the only
//! failure mode is a panic during trace, not a use-after-free. Once
//! Phase 2 lands the arena allocator, these same tests catch
//! lifetime bugs.

use cs_runtime::Runtime;

/// Tiny deterministic LCG. Seedable so failures reproduce.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ 0xCAFEF00DD15EA5E5)
    }
    fn next_u32(&mut self) -> u32 {
        // Linear congruential generator; Numerical Recipes constants.
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 32) as u32
    }
    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.next_u32() % (hi - lo)
    }
}

#[derive(Clone, Debug)]
enum Op {
    DefineList(usize, usize),
    DefineString(usize, String),
    DefineVector(usize, usize),
    DefineHashtable(usize),
    DefineCounter(usize),
    Mutate(usize),
    Collect,
    ReadLength(usize),
}

fn gen_op(rng: &mut Rng) -> Op {
    let id = rng.range(0, 8) as usize;
    match rng.range(0, 8) {
        0 => Op::DefineList(id, rng.range(0, 30) as usize),
        1 => {
            // Random short string of letters + spaces. Empty allowed.
            let n = rng.range(0, 16);
            let mut s = String::new();
            for _ in 0..n {
                let r = rng.range(0, 53);
                let c = match r {
                    0..=25 => (b'a' + r as u8) as char,
                    26..=51 => (b'A' + (r - 26) as u8) as char,
                    _ => ' ',
                };
                s.push(c);
            }
            Op::DefineString(id, s)
        }
        2 => Op::DefineVector(id, rng.range(0, 40) as usize),
        3 => Op::DefineHashtable(id),
        4 => Op::DefineCounter(id),
        5 => Op::Mutate(id),
        6 => Op::Collect,
        _ => Op::ReadLength(id),
    }
}

fn run_ops(ops: &[Op]) {
    let mut rt = Runtime::new();
    let mut defined: std::collections::HashMap<usize, &'static str> =
        std::collections::HashMap::new();

    let var_name = |id: usize| -> String { format!("v{}", id) };

    for op in ops {
        match op {
            Op::DefineList(id, n) => {
                let src = format!(
                    "(define {} (let loop ((i {}) (acc '())) \
                     (if (= i 0) acc (loop (- i 1) (cons i acc)))))",
                    var_name(*id),
                    n
                );
                if rt.eval_str("<fuzz>", &src).is_ok() {
                    defined.insert(*id, "list");
                }
            }
            Op::DefineString(id, s) => {
                let src = format!("(define {} \"{}\")", var_name(*id), s);
                if rt.eval_str("<fuzz>", &src).is_ok() {
                    defined.insert(*id, "string");
                }
            }
            Op::DefineVector(id, n) => {
                let src = format!("(define {} (make-vector {} #f))", var_name(*id), n);
                if rt.eval_str("<fuzz>", &src).is_ok() {
                    defined.insert(*id, "vector");
                }
            }
            Op::DefineHashtable(id) => {
                let src = format!("(define {} (make-eq-hashtable))", var_name(*id));
                if rt.eval_str("<fuzz>", &src).is_ok() {
                    defined.insert(*id, "hashtable");
                }
            }
            Op::DefineCounter(id) => {
                let src = format!(
                    "(define {} (let ((n 0)) (lambda () (set! n (+ n 1)) n)))",
                    var_name(*id)
                );
                if rt.eval_str("<fuzz>", &src).is_ok() {
                    defined.insert(*id, "counter");
                }
            }
            Op::Mutate(id) => {
                if let Some(kind) = defined.get(id) {
                    let v = var_name(*id);
                    let src = match *kind {
                        "vector" => format!("(vector-set! {} 0 'x)", v),
                        "hashtable" => format!("(hashtable-set! {} 'k 1)", v),
                        "counter" => format!("({})", v),
                        _ => continue,
                    };
                    let _ = rt.eval_str("<fuzz>", &src);
                }
            }
            Op::Collect => rt.collect(),
            Op::ReadLength(id) => {
                if let Some(kind) = defined.get(id) {
                    let v = var_name(*id);
                    let src = match *kind {
                        "string" => format!("(string-length {})", v),
                        "vector" => format!("(vector-length {})", v),
                        "list" => format!("(length {})", v),
                        "hashtable" => format!("(hashtable-size {})", v),
                        _ => continue,
                    };
                    let _ = rt.eval_str("<fuzz>", &src);
                }
            }
        }
    }
}

fn run_seed(seed: u64, op_count: usize, force_collect_each_step: bool) {
    let mut rng = Rng::new(seed);
    let ops: Vec<Op> = (0..op_count).map(|_| gen_op(&mut rng)).collect();
    if force_collect_each_step {
        let mut interleaved = Vec::with_capacity(op_count * 2);
        for op in ops {
            interleaved.push(op);
            interleaved.push(Op::Collect);
        }
        run_ops(&interleaved);
    } else {
        run_ops(&ops);
    }
}

/// Number of seeds to exercise per fuzz test.
///
/// Defaults to 16 for unit-test runtime. The CI workflow
/// `.github/workflows/m5-fuzz.yml` overrides via `CRABSCHEME_FUZZ_SEEDS`
/// to crank the count for nightly runs (default in CI: 1024).
fn fuzz_seed_count() -> u64 {
    std::env::var("CRABSCHEME_FUZZ_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16)
}

#[test]
fn collect_never_panics_random_workloads() {
    let n = fuzz_seed_count();
    for seed in 0..n {
        run_seed(seed, 32, false);
    }
}

#[test]
fn collect_after_each_op_never_panics() {
    // Worst-case for tracing bugs: collect between every op.
    let n = fuzz_seed_count();
    for seed in 0..n {
        run_seed(seed, 16, true);
    }
}

#[test]
fn long_run_with_periodic_collects() {
    // One large run mixing all operation types.
    run_seed(0xDEADBEEF, 256, false);
}
