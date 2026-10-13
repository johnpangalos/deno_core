// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2]
fn op_webidl_u8(#[webidl] value: u8) -> u8 {
  value
}

#[op2]
fn op_webidl_optional(#[webidl] value: Option<String>) -> u32 {
  value.map_or(0, |value| value.len() as u32)
}

#[op2]
fn op_webidl_sequence(#[webidl] values: Vec<u8>) -> u32 {
  values.len() as u32
}
