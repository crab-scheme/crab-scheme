//! N-body — Computer Language Benchmarks Game port (warmup-curve variant).
//!
//! Reference implementation mirroring the Scheme port at
//! `bench/microbench/scheme/nbody.scm`. Same algorithm, same initial
//! conditions, same outer warmup-curve loop — so the two can be
//! compared line-by-line by the harness script.
//!
//! Built single-file for `rustc -O`. No deps.

use std::time::Instant;

const PI: f64 = 3.141592653589793;
const SOLAR_MASS: f64 = 4.0 * PI * PI;
const DAYS_PER_YEAR: f64 = 365.24;

#[derive(Clone, Copy)]
struct Body {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    mass: f64,
}

impl Body {
    const fn new(x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, m: f64) -> Self {
        Body {
            x,
            y,
            z,
            vx: vx * DAYS_PER_YEAR,
            vy: vy * DAYS_PER_YEAR,
            vz: vz * DAYS_PER_YEAR,
            mass: m * SOLAR_MASS,
        }
    }
}

fn initial_bodies() -> [Body; 5] {
    [
        // Sun
        Body::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0),
        // Jupiter
        Body::new(
            4.84143144246472090,
            -1.16032004402742839,
            -0.103622044471123109,
            0.00166007664274403694,
            0.00769901118419740425,
            -0.0000690460016972063023,
            0.000954791938424326609,
        ),
        // Saturn
        Body::new(
            8.34336671824457987,
            4.12479856412430479,
            -0.403523417114321381,
            -0.00276742510726862411,
            0.00499852801234917238,
            0.0000230417297573763929,
            0.000285885980666130812,
        ),
        // Uranus
        Body::new(
            12.8943695621391310,
            -15.1111514016986312,
            -0.223307578892655734,
            0.00296460137564761618,
            0.00237847173959480950,
            -0.0000296589568540237556,
            0.0000436624404335156298,
        ),
        // Neptune
        Body::new(
            15.3796971148509165,
            -25.9193146099879641,
            0.179258772950371181,
            0.00268067772490389322,
            0.00162824170038242295,
            -0.0000951592254519715870,
            0.0000515138902046611451,
        ),
    ]
}

fn offset_momentum(bodies: &mut [Body; 5]) {
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for b in bodies.iter() {
        px += b.vx * b.mass;
        py += b.vy * b.mass;
        pz += b.vz * b.mass;
    }
    bodies[0].vx = -px / SOLAR_MASS;
    bodies[0].vy = -py / SOLAR_MASS;
    bodies[0].vz = -pz / SOLAR_MASS;
}

fn energy(bodies: &[Body; 5]) -> f64 {
    let mut e = 0.0;
    let n = bodies.len();
    for i in 0..n {
        let bi = &bodies[i];
        e += 0.5 * bi.mass * (bi.vx * bi.vx + bi.vy * bi.vy + bi.vz * bi.vz);
        for j in (i + 1)..n {
            let bj = &bodies[j];
            let dx = bi.x - bj.x;
            let dy = bi.y - bj.y;
            let dz = bi.z - bj.z;
            let d = (dx * dx + dy * dy + dz * dz).sqrt();
            e -= (bi.mass * bj.mass) / d;
        }
    }
    e
}

#[inline(always)]
fn advance(bodies: &mut [Body; 5], dt: f64) {
    let n = bodies.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = bodies[i].x - bodies[j].x;
            let dy = bodies[i].y - bodies[j].y;
            let dz = bodies[i].z - bodies[j].z;
            let d2 = dx * dx + dy * dy + dz * dz;
            let d = d2.sqrt();
            let mag = dt / (d2 * d);
            let mi = bodies[i].mass;
            let mj = bodies[j].mass;
            bodies[i].vx -= dx * mj * mag;
            bodies[i].vy -= dy * mj * mag;
            bodies[i].vz -= dz * mj * mag;
            bodies[j].vx += dx * mi * mag;
            bodies[j].vy += dy * mi * mag;
            bodies[j].vz += dz * mi * mag;
        }
    }
    for b in bodies.iter_mut() {
        b.x += dt * b.vx;
        b.y += dt * b.vy;
        b.z += dt * b.vz;
    }
}

fn warmup_curve(bodies: &mut [Body; 5], rounds: usize, steps_per_round: usize) {
    for k in 0..rounds {
        let t0 = Instant::now();
        for _ in 0..steps_per_round {
            advance(bodies, 0.01);
        }
        let dt = t0.elapsed().as_secs_f64();
        println!("nbody-round {} {}", k, dt);
    }
}

fn main() {
    let mut bodies = initial_bodies();
    offset_momentum(&mut bodies);
    println!("nbody-energy-start {}", energy(&bodies));
    // Matched to the Scheme port: 1500 rounds × 1000 steps = 1.5M
    // total. Rust finishes much faster than Scheme; we keep the
    // schedule identical so the harness sees the same row count.
    warmup_curve(&mut bodies, 1500, 1000);
    println!("nbody-energy-final {}", energy(&bodies));
}
