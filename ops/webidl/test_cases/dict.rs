// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::WebIDL;

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(dictionary)]
struct Dictionary {
  #[webidl(rename = "renamed")]
  foo_bar: u8,
  #[webidl(default = 5)]
  baz: u32,
  #[webidl(required)]
  qux: String,
  #[options(clamp = true)]
  clamped: u8,
  maybe: Option<String>,
  r#type: String,
}
