// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
pub fn op_webidl_arg(#[webidl] foo: u8) {
  let _ = foo;
}

#[op2]
#[string]
pub fn op_webidl_second_arg(
  #[webidl] foo: String,
  #[webidl] bar: Option<bool>,
) -> String {
  let _ = bar;
  foo
}
