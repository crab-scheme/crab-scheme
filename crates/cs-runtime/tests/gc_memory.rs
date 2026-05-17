#![cfg(not(feature = "countable-memory"))]

//! Memory-usage baseline for the M5 exit gate.
//!
//! The M5 spec asserts: "Memory usage on representative programs no
//! worse than RC + cycle collector" — i.e. peak resident memory under
//! the GC must be ≤ 1.2× the M4 RC baseline.
//!
//! Phase 1 IS the M4 baseline (Gc<T> still backed by Rc<Slot<T>>), so
//! this test mostly exists to establish the measurement infrastructure
//! Phase 2 will use as a regression gate. Today's numbers are the
//! reference; Phase 2's commit must keep this comparison ≤ 1.2×.
//!
//! Platform support: Linux + macOS via libc. On other platforms the
//! test is skipped (returns 0 which always passes).

use cs_runtime::Runtime;

/// Peak RSS in bytes since process start. Returns `None` on platforms
/// we don't have a probe for, in which case the test that uses this
/// becomes a no-op.
#[cfg(target_os = "macos")]
fn peak_rss() -> Option<usize> {
    use std::mem::MaybeUninit;
    extern "C" {
        fn task_info(target: u32, flavor: i32, info: *mut u8, count: *mut u32) -> i32;
        fn mach_task_self() -> u32;
    }
    // mach/task_info.h: TASK_BASIC_INFO_64 = 5
    // The struct is mach_task_basic_info { virtual_size, resident_size_max,
    //   resident_size, ... }. resident_size_max field is at offset 16
    // bytes (after virtual_size: u64 and one filler u64), but the
    // simplest way is the standard `proc_pidinfo` rusage_v2.
    //
    // For this test we use getrusage instead — it's portable and gives
    // ru_maxrss in bytes on macOS (in KB on Linux).
    let _ = (task_info, mach_task_self);
    let mut ru: libc_rusage = unsafe { MaybeUninit::zeroed().assume_init() };
    let r = unsafe {
        getrusage_extern(0 /* RUSAGE_SELF */, &mut ru)
    };
    if r == 0 {
        // macOS reports ru_maxrss in BYTES.
        Some(ru.ru_maxrss as usize)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn peak_rss() -> Option<usize> {
    use std::mem::MaybeUninit;
    let mut ru: libc_rusage = unsafe { MaybeUninit::zeroed().assume_init() };
    let r = unsafe {
        getrusage_extern(0 /* RUSAGE_SELF */, &mut ru)
    };
    if r == 0 {
        // Linux reports ru_maxrss in KILOBYTES.
        Some((ru.ru_maxrss as usize).saturating_mul(1024))
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peak_rss() -> Option<usize> {
    None
}

// Minimal struct mirror of `struct rusage` — we only read ru_maxrss
// so layout-after-the-first-two-timeval-fields is enough. Both Linux
// and macOS keep ru_maxrss as the third (long) field.
#[repr(C)]
struct libc_rusage {
    ru_utime_sec: i64,
    ru_utime_usec: i64,
    ru_stime_sec: i64,
    ru_stime_usec: i64,
    ru_maxrss: i64,
    _padding: [i64; 14],
}

extern "C" {
    #[link_name = "getrusage"]
    fn getrusage_extern(who: i32, usage: *mut libc_rusage) -> i32;
}

fn fmt_bytes(b: usize) -> String {
    if b >= 1 << 20 {
        format!("{:.2} MiB", b as f64 / (1 << 20) as f64)
    } else if b >= 1 << 10 {
        format!("{:.2} KiB", b as f64 / (1 << 10) as f64)
    } else {
        format!("{} B", b)
    }
}

#[test]
fn memory_baseline_factorial_recursion() {
    let Some(start) = peak_rss() else {
        eprintln!("memory_baseline: peak_rss unsupported on this platform; skipping");
        return;
    };
    let mut rt = Runtime::new();
    rt.eval_str(
        "<bench>",
        r#"
          (define (fact n)
            (if (= n 0) 1 (* n (fact (- n 1)))))
          (define _r (fact 200))
        "#,
    )
    .expect("factorial 200 evaluates");
    rt.collect();
    let after = peak_rss().expect("peak_rss after");
    eprintln!(
        "factorial(200): start={} after={} delta={}",
        fmt_bytes(start),
        fmt_bytes(after),
        fmt_bytes(after.saturating_sub(start))
    );
    // 80 MiB is a generous ceiling — Phase 1's Rc-backed runtime
    // typically lands around 12-30 MiB on this workload. Phase 2's
    // arena should be similar or lower; we'll tighten this when
    // Phase 2 lands.
    assert!(
        after < 80 * (1 << 20),
        "factorial(200) peak RSS {} exceeded 80 MiB ceiling",
        fmt_bytes(after)
    );
}

#[test]
fn memory_baseline_large_list_construction() {
    let Some(start) = peak_rss() else {
        return;
    };
    let mut rt = Runtime::new();
    rt.eval_str(
        "<bench>",
        r#"
          (define (range n)
            (let loop ((i n) (acc '()))
              (if (= i 0) acc (loop (- i 1) (cons i acc)))))
          (define lst (range 10000))
          (define s (apply + lst))
        "#,
    )
    .expect("10k list works");
    rt.collect();
    let after = peak_rss().expect("peak_rss after");
    eprintln!(
        "10k-list: start={} after={} delta={}",
        fmt_bytes(start),
        fmt_bytes(after),
        fmt_bytes(after.saturating_sub(start))
    );
    assert!(
        after < 80 * (1 << 20),
        "10k-list peak RSS {} exceeded 80 MiB ceiling",
        fmt_bytes(after)
    );
}

#[test]
fn memory_baseline_repeated_runtime_creation() {
    // Build and drop 10 Runtimes back-to-back. Each is supposed to
    // free fully on drop. If this leaks badly we'd see RSS climb
    // monotonically.
    let Some(start) = peak_rss() else {
        return;
    };
    for _ in 0..10 {
        let mut rt = Runtime::new();
        rt.eval_str("<bench>", "(define s (make-string 1000 #\\x))")
            .unwrap();
        rt.collect();
        drop(rt);
    }
    let after = peak_rss().expect("peak_rss after");
    eprintln!(
        "10 fresh runtimes: start={} after={} delta={}",
        fmt_bytes(start),
        fmt_bytes(after),
        fmt_bytes(after.saturating_sub(start))
    );
    // Even if each leaks a few KB this should stay well under any
    // reasonable ceiling.
    assert!(
        after < 80 * (1 << 20),
        "10 fresh Runtimes peak RSS {} exceeded 80 MiB ceiling",
        fmt_bytes(after)
    );
}
