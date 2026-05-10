// Reference impl mirroring scheme/mandelbrot.scm.
// Same termination criterion (50 iters), same N.

fn pixel(cr: f64, ci: f64) -> bool {
    let mut zr = 0.0_f64;
    let mut zi = 0.0_f64;
    for _ in 0..50 {
        if zr * zr + zi * zi > 4.0 {
            return false;
        }
        let nzr = zr * zr - zi * zi + cr;
        let nzi = 2.0 * zr * zi + ci;
        zr = nzr;
        zi = nzi;
    }
    true
}

fn mandelbrot(n: usize) -> u64 {
    let mut count = 0u64;
    for y in 0..n {
        for x in 0..n {
            let cr = 2.0 * x as f64 / n as f64 - 1.5;
            let ci = 2.0 * y as f64 / n as f64 - 1.0;
            if pixel(cr, ci) {
                count += 1;
            }
        }
    }
    count
}

fn main() {
    let n = 80;
    println!("mandelbrot({}) = {}", n, mandelbrot(n));
}
