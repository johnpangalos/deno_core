// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::WebIDL;

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Foo {
  pub foo_bar: u32,
  #[webidl(rename = "baz")]
  pub qux: bool,
  #[webidl(default = 5)]
  #[options(clamp = true)]
  pub with_default: u8,
  pub maybe: Option<String>,
  #[webidl(required)]
  pub must: Option<String>,
  pub r#type: bool,
}
