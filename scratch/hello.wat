(module
  (import "env" "log" (func $log (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello from wasm")
  (func (export "frog_main")
    i32.const 0
    i32.const 15
    call $log))
