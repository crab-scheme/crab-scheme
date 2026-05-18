; Conformance test for `(crab collection)` — stdlib-modules iter 12.

(test-section "(crab collection) — queue")

(define __q__ (queue-new))
(test-true "empty queue is empty?" (queue-empty? __q__))
(test-eqv  "empty queue length is 0" 0 (queue-length __q__))
(test-false "pop on empty queue returns #f" (queue-pop! __q__))
(test-false "peek on empty queue returns #f" (queue-peek __q__))

(queue-push! __q__ "first")
(queue-push! __q__ "second")
(queue-push! __q__ "third")

(test-eqv  "length after 3 pushes"      3      (queue-length __q__))
(test-equal "peek returns first" "first" (queue-peek __q__))
(test-equal "pop returns first"  "first" (queue-pop! __q__))
(test-equal "pop returns second" "second" (queue-pop! __q__))
(test-eqv  "length after 2 pops"        1      (queue-length __q__))

(test-section "(crab collection) — set")

(define __s__ (set-new))
(test-eqv "empty set size is 0" 0 (set-size __s__))
(test-false "contains? on missing" (set-contains? __s__ "foo"))

(set-add! __s__ "foo")
(set-add! __s__ "bar")
(set-add! __s__ "foo")  ; duplicate
(test-eqv  "size after 2 unique adds" 2     (set-size __s__))
(test-true "contains foo"             (set-contains? __s__ "foo"))
(test-true "contains bar"             (set-contains? __s__ "bar"))

(test-true  "remove returns true on hit"   (set-remove! __s__ "foo"))
(test-false "remove returns false on miss" (set-remove! __s__ "foo"))
(test-false "removed not contained"        (set-contains? __s__ "foo"))

(test-equal "set->list returns lone element"
            '("bar")
            (set->list __s__))

(test-section "(crab collection) — heap (max-heap)")

(define __h__ (heap-new))
(test-false "pop on empty returns #f" (heap-pop! __h__))

(heap-push! __h__ 3)
(heap-push! __h__ 1)
(heap-push! __h__ 4)
(heap-push! __h__ 1)
(heap-push! __h__ 5)
(heap-push! __h__ 9)
(heap-push! __h__ 2)
(heap-push! __h__ 6)

(test-eqv "heap-peek returns max"  9 (heap-peek __h__))
(test-eqv "pop order: 9"           9 (heap-pop! __h__))
(test-eqv "pop order: 6"           6 (heap-pop! __h__))
(test-eqv "pop order: 5"           5 (heap-pop! __h__))
(test-eqv "pop order: 4"           4 (heap-pop! __h__))
(test-eqv "length after 4 pops"    4 (heap-length __h__))

(test-section "(crab collection) — type clash")

(test-true "queue op on heap raises"
           (guard (e (#t #t))
             (queue-push! __h__ "x")
             #f))
