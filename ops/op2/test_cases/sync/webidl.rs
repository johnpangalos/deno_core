// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;
use deno_core::OpState;
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Options {
  _flag: bool,
}

#[op2]
pub fn op_webidl(
  #[webidl] _s: String,
  _state: &mut OpState,
  #[webidl] _n: Option<u32>,
  #[webidl] _options: Options,
  #[webidl] _any: v8::Local<v8::Value>,
) {
}
