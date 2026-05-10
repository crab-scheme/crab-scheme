// Reference impl mirroring scheme/nqueens.scm.

fn safe(row: i64, col: i64, placed: &[(i64, i64)]) -> bool {
    for &(r, c) in placed {
        if c == col || (r - row) == (c - col) || (r - row) == (col - c) {
            return false;
        }
    }
    true
}

fn place(row: i64, n: i64, placed: &mut Vec<(i64, i64)>) -> u64 {
    if row > n {
        return 1;
    }
    let mut count = 0u64;
    for col in 1..=n {
        if safe(row, col, placed) {
            placed.push((row, col));
            count += place(row + 1, n, placed);
            placed.pop();
        }
    }
    count
}

fn main() {
    let mut placed = Vec::new();
    println!("nqueens(8) = {}", place(1, 8, &mut placed));
}
