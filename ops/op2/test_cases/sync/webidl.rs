// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dictionary {
  #[webidl(rename = "count", default = 1)]
  #[options(clamp = true)]
  pub count: u8,
}

#[op2]
pub fn op_webidl_dictionary(#[webidl] dictionary: Dictionary) -> u8 {
  dictionary.count
}

#[op2]
pub fn op_webidl_scalar(#[webidl] value: u8, #[webidl] other: String) -> u8 {
  value + other.len() as u8
}
