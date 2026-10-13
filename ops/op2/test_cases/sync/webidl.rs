// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
pub fn op_webidl_arg(
  #[webidl] foo: Option<u8>,
  #[webidl] bar: Vec<String>,
  #[webidl] baz: u32,
) {
  let _ = (foo, bar, baz);
}
