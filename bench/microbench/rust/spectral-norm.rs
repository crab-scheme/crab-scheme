// Reference impl mirroring scheme/spectral-norm.scm.

fn matrix_elt(i: usize, j: usize) -> f64 {
    let ij = i + j;
    let denom = (ij * (ij + 1)) / 2 + i + 1;
    1.0 / denom as f64
}

fn mul_a_v(n: usize, v: &[f64], out: &mut [f64]) {
    for i in 0..n {
        let mut s = 0.0;
        for j in 0..n {
            s += matrix_elt(i, j) * v[j];
        }
        out[i] = s;
    }
}

fn mul_at_v(n: usize, v: &[f64], out: &mut [f64]) {
    for i in 0..n {
        let mut s = 0.0;
        for j in 0..n {
            s += matrix_elt(j, i) * v[j];
        }
        out[i] = s;
    }
}

fn mul_at_a_v(n: usize, v: &[f64], out: &mut [f64], tmp: &mut [f64]) {
    mul_a_v(n, v, tmp);
    mul_at_v(n, tmp, out);
}

fn spectral_norm(n: usize) -> f64 {
    let mut u = vec![1.0_f64; n];
    let mut v = vec![0.0_f64; n];
    let mut tmp = vec![0.0_f64; n];
    for _ in 0..10 {
        mul_at_a_v(n, &u, &mut v, &mut tmp);
        mul_at_a_v(n, &v, &mut u, &mut tmp);
    }
    let mut v_bv = 0.0;
    let mut vv = 0.0;
    for i in 0..n {
        v_bv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    (v_bv / vv).sqrt()
}

fn main() {
    let n = 50;
    println!("spectral-norm({}) = {}", n, spectral_norm(n));
}
