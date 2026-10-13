// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use std::collections::HashMap;

#[op2]
pub fn op_webidl_string(#[webidl] s: String) {
  let _ = s;
}

#[op2]
pub fn op_webidl_option_vec(
  #[webidl] v: Option<Vec<u8>>,
  #[webidl] m: HashMap<String, u32>,
) {
  let _ = (v, m);
}
