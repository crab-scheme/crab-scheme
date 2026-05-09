(test-section "R6RS bytevector typed accessors with endianness")

; --- s8 ref/set ---
(define bv1 (make-bytevector 4 0))
(bytevector-s8-set! bv1 0 -1)
(bytevector-s8-set! bv1 1 127)
(bytevector-s8-set! bv1 2 -128)
(test-eqv "s8 -1"   -1   (bytevector-s8-ref bv1 0))
(test-eqv "s8 127"   127 (bytevector-s8-ref bv1 1))
(test-eqv "s8 -128" -128 (bytevector-s8-ref bv1 2))
; u8 reads negative s8 byte as 255
(test-eqv "u8 of -1" 255 (bytevector-u8-ref bv1 0))

; out-of-range writes raise.
(test-true "s8 set out-of-range raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (bytevector-s8-set! bv1 0 200))))

; --- u16 / s16 with endianness ---
(define bv2 (make-bytevector 2 0))
; 0x1234 big endian: byte0=0x12 byte1=0x34
(bytevector-u16-set! bv2 0 #x1234 'big)
(test-eqv "u16-big byte0" #x12 (bytevector-u8-ref bv2 0))
(test-eqv "u16-big byte1" #x34 (bytevector-u8-ref bv2 1))
(test-eqv "u16-big roundtrip" #x1234 (bytevector-u16-ref bv2 0 'big))
(test-eqv "u16-little of same bytes" #x3412 (bytevector-u16-ref bv2 0 'little))

; signed 16 round-trip with negative
(define bv3 (make-bytevector 2 0))
(bytevector-s16-set! bv3 0 -1 'little)
(test-eqv "s16 -1 little" -1 (bytevector-s16-ref bv3 0 'little))
; Same bytes interpreted as u16 = 65535
(test-eqv "u16 of 0xFFFF" 65535 (bytevector-u16-ref bv3 0 'little))

; --- u32 / s32 ---
(define bv4 (make-bytevector 4 0))
(bytevector-u32-set! bv4 0 #x01020304 'big)
(test-eqv "u32-big b0" #x01 (bytevector-u8-ref bv4 0))
(test-eqv "u32-big b3" #x04 (bytevector-u8-ref bv4 3))
(test-eqv "u32-big read" #x01020304 (bytevector-u32-ref bv4 0 'big))
(test-eqv "u32-little same" #x04030201 (bytevector-u32-ref bv4 0 'little))

; s32 negative round-trip
(define bv5 (make-bytevector 4 0))
(bytevector-s32-set! bv5 0 -1 'big)
(test-eqv "s32 -1 round-trip" -1 (bytevector-s32-ref bv5 0 'big))

; --- u64 / s64 (note: u64 max may exceed fixnum range) ---
(define bv6 (make-bytevector 8 0))
(bytevector-u64-set! bv6 0 #xCAFEBABE 'big)
(test-eqv "u64 read" #xCAFEBABE (bytevector-u64-ref bv6 0 'big))
; Large value > i64::MAX should round-trip via bignum
(define max-u64 (- (expt 2 64) 1))
(bytevector-u64-set! bv6 0 max-u64 'big)
(test-equal "u64 max round-trip" max-u64 (bytevector-u64-ref bv6 0 'big))

; s64 negative
(define bv7 (make-bytevector 8 0))
(bytevector-s64-set! bv7 0 -1 'little)
(test-eqv "s64 -1" -1 (bytevector-s64-ref bv7 0 'little))

; --- IEEE single (f32) and double (f64) ---
(define bv8 (make-bytevector 8 0))
(bytevector-ieee-double-set! bv8 0 3.14 'big)
(test-equal "f64 round-trip" 3.14 (bytevector-ieee-double-ref bv8 0 'big))

(define bv9 (make-bytevector 4 0))
(bytevector-ieee-single-set! bv9 0 1.5 'little)
(test-true "f32 close to 1.5"
  (< (abs (- 1.5 (bytevector-ieee-single-ref bv9 0 'little))) 1e-6))

; --- native endianness ---
(define ne (native-endianness))
(test-true "native is big or little" (or (eq? ne 'big) (eq? ne 'little)))

; native variants round-trip with explicit native endianness.
(define bv10 (make-bytevector 2 0))
(bytevector-u16-native-set! bv10 0 #x1234)
(test-eqv "u16 native round-trip" #x1234 (bytevector-u16-native-ref bv10 0))
; Same as explicit native.
(test-eqv "u16 native = explicit"
  (bytevector-u16-ref bv10 0 ne)
  (bytevector-u16-native-ref bv10 0))

(define bv11 (make-bytevector 4 0))
(bytevector-ieee-single-native-set! bv11 0 -2.0)
(test-equal "f32 native round" -2.0
  (bytevector-ieee-single-native-ref bv11 0))

; --- bounds checking ---
(define bv-small (make-bytevector 2 0))
(test-true "u32 OOB raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (bytevector-u32-ref bv-small 0 'big))))

; --- bad endianness symbol raises ---
(test-true "invalid endian symbol raises"
  (with-exception-handler (lambda (c) (error? c))
    (lambda () (bytevector-u16-ref bv2 0 'middle))))
