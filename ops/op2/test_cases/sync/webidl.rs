// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  #[webidl(default = 1)]
  foo_bar: u8,
}

#[derive(WebIDL)]
#[webidl(enum)]
pub enum Mode {
  ReadOnly,
  #[webidl(rename = "rw")]
  ReadWrite,
}

#[op2]
pub fn op_webidl_arg(
  #[webidl] x: u8,
  #[webidl] dict: Dict,
  #[webidl] mode: Option<Mode>,
  #[webidl] values: Vec<String>,
) {
  let _ = (x, dict.foo_bar, mode, values);
}
