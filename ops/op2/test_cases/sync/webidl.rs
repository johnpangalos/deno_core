// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use std::collections::HashMap;

use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  pub foo: String,
}

#[op2]
pub fn op_webidl_u8(#[webidl] value: u8) -> u32 {
  value as u32
}

#[op2]
pub fn op_webidl_string(#[webidl] value: String) -> u32 {
  value.len() as u32
}

#[op2]
pub fn op_webidl_option(#[webidl] value: Option<String>) -> bool {
  value.is_some()
}

#[op2]
pub fn op_webidl_vec(#[webidl] value: Vec<u8>) -> u32 {
  value.len() as u32
}

#[op2]
pub fn op_webidl_map(#[webidl] value: HashMap<String, String>) -> u32 {
  value.len() as u32
}

#[op2]
pub fn op_webidl_dict(#[webidl] value: Dict) -> u32 {
  value.foo.len() as u32
}
