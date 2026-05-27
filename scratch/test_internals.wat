;; Internals 2a cross-brick gate: proves find_class (positive + negative) and
;; field_info (exact offset + tc→ValType mapping) through the il2cpp.* host ABI,
;; checked against known internals.txt values for Pixel Worlds' `Player` class:
;;   isDeadTimeCounter : System.Single @ Offset 0x60  ->  field_info == (F32(=8)<<32)|0x60
(module
  (import "env" "log" (func $log (param i32 i32)))
  (import "il2cpp" "find_class" (func $find_class (param i32 i32) (result i64)))
  (import "il2cpp" "field_info" (func $field_info (param i64 i32 i32) (result i64)))
  (memory (export "memory") 1)

  (data (i32.const 0)   "Player")
  (data (i32.const 16)  "isDeadTimeCounter")
  (data (i32.const 48)  "ZzNoSuchClass")
  (data (i32.const 64)  "find_class Player: ok")
  (data (i32.const 96)  "find_class Player: FAIL")
  (data (i32.const 128) "field_info Single: ok (0x60,F32)")
  (data (i32.const 176) "field_info Single: MISMATCH")
  (data (i32.const 208) "negative find_class: ok (0)")
  (data (i32.const 240) "negative find_class: FAIL")

  (func (export "frog_main")
    (local $klass i64)
    (local $info i64)
    (local $neg i64)

    ;; 1) find_class("Player") -> non-zero
    (local.set $klass (call $find_class (i32.const 0) (i32.const 6)))
    (if (i64.ne (local.get $klass) (i64.const 0))
      (then (call $log (i32.const 64) (i32.const 21)))
      (else (call $log (i32.const 96) (i32.const 23))))

    ;; 2) field_info(Player, "isDeadTimeCounter") == (F32<<32)|0x60 == 0x800000060
    (local.set $info (call $field_info (local.get $klass) (i32.const 16) (i32.const 17)))
    (if (i64.eq (local.get $info) (i64.const 0x800000060))
      (then (call $log (i32.const 128) (i32.const 32)))
      (else (call $log (i32.const 176) (i32.const 27))))

    ;; 3) find_class("ZzNoSuchClass") == 0
    (local.set $neg (call $find_class (i32.const 48) (i32.const 13)))
    (if (i64.eq (local.get $neg) (i64.const 0))
      (then (call $log (i32.const 208) (i32.const 27)))
      (else (call $log (i32.const 240) (i32.const 25))))
  )
)
