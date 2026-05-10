// Reference impl mirroring scheme/binary-trees.scm. Boxed-pair allocation
// (no arena) so we exercise the same allocator path the Scheme tier uses.

enum Tree {
    Leaf,
    Node(Box<Tree>, Box<Tree>),
}

fn make_tree(depth: u64) -> Tree {
    if depth == 0 {
        Tree::Leaf
    } else {
        Tree::Node(
            Box::new(make_tree(depth - 1)),
            Box::new(make_tree(depth - 1)),
        )
    }
}

fn check_tree(t: &Tree) -> u64 {
    match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => 1 + check_tree(l) + check_tree(r),
    }
}

fn run(depth: u64) -> u64 {
    let mut acc = 0u64;
    let mut d = 4;
    while d <= depth {
        let iters: u64 = 1 << (depth - d + 4);
        let mut sum = 0u64;
        for _ in 0..iters {
            sum += check_tree(&make_tree(d));
        }
        acc += sum;
        d += 2;
    }
    acc
}

fn main() {
    let depth = 10;
    println!("binary-trees({}) = {}", depth, run(depth));
}
