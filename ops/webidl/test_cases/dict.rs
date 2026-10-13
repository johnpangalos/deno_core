// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::v8;
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  a: u8,
  #[options(clamp = true)]
  b: Vec<u16>,
  #[webidl(default = Some(3))]
  #[options(enforce_range = true)]
  c: Option<u32>,
  #[webidl(rename = "e")]
  d: u16,
  f: std::collections::HashMap<String, u32>,
  #[webidl(required)]
  g: Option<u32>,
  some_long_name: String,
  r#type: Option<String>,
}

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct WithLifetime<'s> {
  any: v8::Local<'s, v8::Value>,
}
