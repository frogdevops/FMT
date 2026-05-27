;; External brick integration test: exercises the mem.* read ABI end-to-end.
;; Proves: regions() writes records into guest memory, read() returns a typed
;; value from a real address, and a delusional address is rejected (not a crash).
(module
  (import "env" "log" (func $log (param i32 i32)))
  ;; mem.read(addr:i64, ty:i32, len:i32, out_ptr:i32, out_cap:i32) -> i32
  (import "mem" "read" (func $read (param i64 i32 i32 i32 i32) (result i32)))
  ;; mem.regions(out_ptr:i32, out_cap_count:i32) -> i32
  (import "mem" "regions" (func $regions (param i32 i32) (result i32)))
  (memory (export "memory") 2)

  (data (i32.const 0)  "regions ok")            ;; len 10
  (data (i32.const 16) "regions empty")         ;; len 13
  (data (i32.const 32) "read ok")               ;; len 7
  (data (i32.const 48) "read failed")           ;; len 11
  (data (i32.const 64) "bad-read rejected")     ;; len 17
  (data (i32.const 96) "bad-read NOT rejected") ;; len 21

  (func (export "frog_main")
    (local $count i32)
    (local $base i64)
    (local $r i32)

    ;; 1) regions(out=1024, cap=4): host writes up to 4 records {u64 base,u64 size,u32 prot}
    (local.set $count (call $regions (i32.const 1024) (i32.const 4)))
    (if (i32.gt_s (local.get $count) (i32.const 0))
      (then (call $log (i32.const 0) (i32.const 10)))    ;; "regions ok"
      (else (call $log (i32.const 16) (i32.const 13))))  ;; "regions empty"

    ;; load first region's base (u64 LE at offset 0 of the first record)
    (local.set $base (i64.load (i32.const 1024)))

    ;; 2) read(base, U32(tag=2), len=4, out=2048, cap=4): real readable address
    (local.set $r (call $read (local.get $base) (i32.const 2) (i32.const 4) (i32.const 2048) (i32.const 4)))
    (if (i32.ge_s (local.get $r) (i32.const 0))
      (then (call $log (i32.const 32) (i32.const 7)))    ;; "read ok"
      (else (call $log (i32.const 48) (i32.const 11))))  ;; "read failed"

    ;; 3) bad read: address 0x10 must be rejected with a negative status, never crash
    (local.set $r (call $read (i64.const 0x10) (i32.const 2) (i32.const 4) (i32.const 2048) (i32.const 4)))
    (if (i32.lt_s (local.get $r) (i32.const 0))
      (then (call $log (i32.const 64) (i32.const 17)))   ;; "bad-read rejected"
      (else (call $log (i32.const 96) (i32.const 21))))  ;; "bad-read NOT rejected"
  )
)
