// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[op2(async)]
pub async fn op_webidl_async_arg(#[webidl] value: u8, #[webidl] name: Option<String>) {
  let _ = (value, name);
}
