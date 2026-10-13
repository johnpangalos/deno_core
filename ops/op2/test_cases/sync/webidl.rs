// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
fn op_webidl(#[webidl] s: String, #[webidl] n: u32) {
  let _ = s;
  let _ = n;
}
