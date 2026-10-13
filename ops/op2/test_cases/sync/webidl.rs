// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
pub fn op_webidl(
  #[webidl] s: String,
  #[webidl] n: u32,
  #[webidl] b: bool,
  #[webidl] f: f64,
  #[webidl] o: Option<String>,
  #[webidl] v: Vec<u8>,
  #[webidl] m: std::collections::HashMap<String, u32>,
) -> u32 {
  n
}
