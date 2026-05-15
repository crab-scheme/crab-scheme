; N-body — Computer Language Benchmarks Game port.
; Models a planetary system (Sun + Jupiter + Saturn + Uranus + Neptune)
; under Newtonian gravity; advances the system one step at a time using
; symplectic integration. The hot inner loop is pairwise force update —
; pure f64 arithmetic, no allocation, deeply repetitive.
;
; Each body is a 7-element f64 vector: [x y z vx vy vz mass].
;
; Outer "warmup curve" loop: runs `outer` rounds of `inner` advance
; steps each, printing time per round to stdout. Each round is a fixed
; unit of work; reading the timings shows the JIT warmup transition.

(define pi 3.141592653589793)
(define solar-mass (* 4.0 pi pi))
(define days-per-year 365.24)

; Body accessor + setter shorthands.
(define (bx b) (vector-ref b 0))
(define (by b) (vector-ref b 1))
(define (bz b) (vector-ref b 2))
(define (bvx b) (vector-ref b 3))
(define (bvy b) (vector-ref b 4))
(define (bvz b) (vector-ref b 5))
(define (bmass b) (vector-ref b 6))
(define (set-bx! b v) (vector-set! b 0 v))
(define (set-by! b v) (vector-set! b 1 v))
(define (set-bz! b v) (vector-set! b 2 v))
(define (set-bvx! b v) (vector-set! b 3 v))
(define (set-bvy! b v) (vector-set! b 4 v))
(define (set-bvz! b v) (vector-set! b 5 v))

(define (mkbody x y z vx vy vz m)
  (let ((b (make-vector 7 0.0)))
    (set-bx! b x) (set-by! b y) (set-bz! b z)
    (set-bvx! b (* vx days-per-year))
    (set-bvy! b (* vy days-per-year))
    (set-bvz! b (* vz days-per-year))
    (vector-set! b 6 (* m solar-mass))
    b))

; CLBG initial conditions (sun + 4 outer planets, AU + AU/day units).
(define sun     (mkbody 0.0 0.0 0.0 0.0 0.0 0.0 1.0))
(define jupiter (mkbody 4.84143144246472090
                        -1.16032004402742839
                        -0.103622044471123109
                        0.00166007664274403694
                        0.00769901118419740425
                        -0.0000690460016972063023
                        0.000954791938424326609))
(define saturn  (mkbody 8.34336671824457987
                        4.12479856412430479
                        -0.403523417114321381
                        -0.00276742510726862411
                        0.00499852801234917238
                        0.0000230417297573763929
                        0.000285885980666130812))
(define uranus  (mkbody 12.8943695621391310
                        -15.1111514016986312
                        -0.223307578892655734
                        0.00296460137564761618
                        0.00237847173959480950
                        -0.0000296589568540237556
                        0.0000436624404335156298))
(define neptune (mkbody 15.3796971148509165
                        -25.9193146099879641
                        0.179258772950371181
                        0.00268067772490389322
                        0.00162824170038242295
                        -0.0000951592254519715870
                        0.0000515138902046611451))

(define bodies (vector sun jupiter saturn uranus neptune))
(define nbodies 5)

; Offset solar momentum so the system's center of mass stays put.
(define (offset-momentum!)
  (let loop ((i 0) (px 0.0) (py 0.0) (pz 0.0))
    (if (= i nbodies)
        (begin
          (set-bvx! sun (/ (- px) solar-mass))
          (set-bvy! sun (/ (- py) solar-mass))
          (set-bvz! sun (/ (- pz) solar-mass)))
        (let ((b (vector-ref bodies i)))
          (loop (+ i 1)
                (+ px (* (bvx b) (bmass b)))
                (+ py (* (bvy b) (bmass b)))
                (+ pz (* (bvz b) (bmass b))))))))

; Total energy (kinetic + potential) — used as a correctness anchor.
(define (energy)
  (let outer ((i 0) (e 0.0))
    (if (= i nbodies)
        e
        (let ((bi (vector-ref bodies i)))
          (let* ((ke (* 0.5 (bmass bi)
                       (+ (* (bvx bi) (bvx bi))
                          (* (bvy bi) (bvy bi))
                          (* (bvz bi) (bvz bi))))))
            (let inner ((j (+ i 1)) (pe 0.0))
              (if (= j nbodies)
                  (outer (+ i 1) (- (+ e ke) pe))
                  (let* ((bj (vector-ref bodies j))
                         (dx (- (bx bi) (bx bj)))
                         (dy (- (by bi) (by bj)))
                         (dz (- (bz bi) (bz bj)))
                         (d (sqrt (+ (* dx dx) (* dy dy) (* dz dz)))))
                    (inner (+ j 1) (+ pe (/ (* (bmass bi) (bmass bj)) d)))))))))))

; advance one timestep `dt`.
(define (advance dt)
  (let outer ((i 0))
    (if (= i nbodies)
        ; second pass: update positions.
        (let upd ((k 0))
          (if (= k nbodies)
              'done
              (let ((b (vector-ref bodies k)))
                (set-bx! b (+ (bx b) (* dt (bvx b))))
                (set-by! b (+ (by b) (* dt (bvy b))))
                (set-bz! b (+ (bz b) (* dt (bvz b))))
                (upd (+ k 1)))))
        (let ((bi (vector-ref bodies i)))
          (let inner ((j (+ i 1)))
            (if (= j nbodies)
                (outer (+ i 1))
                (let* ((bj (vector-ref bodies j))
                       (dx (- (bx bi) (bx bj)))
                       (dy (- (by bi) (by bj)))
                       (dz (- (bz bi) (bz bj)))
                       (d2 (+ (* dx dx) (* dy dy) (* dz dz)))
                       (d (sqrt d2))
                       (mag (/ dt (* d2 d)))
                       (mi (bmass bi))
                       (mj (bmass bj)))
                  (set-bvx! bi (- (bvx bi) (* dx mj mag)))
                  (set-bvy! bi (- (bvy bi) (* dy mj mag)))
                  (set-bvz! bi (- (bvz bi) (* dz mj mag)))
                  (set-bvx! bj (+ (bvx bj) (* dx mi mag)))
                  (set-bvy! bj (+ (bvy bj) (* dy mi mag)))
                  (set-bvz! bj (+ (bvz bj) (* dz mi mag)))
                  (inner (+ j 1)))))))))

; Run `n` advance steps with dt = 0.01.
(define (run-steps n)
  (let loop ((i 0))
    (if (< i n)
        (begin (advance 0.01) (loop (+ i 1)))
        'done)))

; Warmup-curve outer loop: print `(round, seconds)` for each of
; `rounds` rounds of `steps-per-round` advance calls.
(define (warmup-curve rounds steps-per-round)
  (let loop ((k 0))
    (if (< k rounds)
        (let ((t0 (current-second)))
          (run-steps steps-per-round)
          (let ((dt (- (current-second) t0)))
            (display "nbody-round ") (display k) (display " ") (display dt) (newline)
            (loop (+ k 1))))
        'done)))

(offset-momentum!)
(display "nbody-energy-start ") (display (energy)) (newline)
; The JIT tier-up threshold is 1024 calls, so a round of 1000 steps
; leaves `advance` un-JIT'd; round 1 crosses the threshold. Total
; 1500 rounds × 1000 steps = 1.5M advance calls ~= 60s on vm-jit.
; The first 1-3 rounds expose the cold → hot transition; subsequent
; rounds give the steady-state JIT throughput.
(warmup-curve 1500 1000)
(display "nbody-energy-final ") (display (energy)) (newline)
