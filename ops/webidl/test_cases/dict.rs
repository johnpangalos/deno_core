// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  a: u8,
  #[options(clamp = true)]
  b: Vec<u16>,
  #[webidl(default = Some(3))]
  c: Option<u32>,
  #[webidl(rename = "e")]
  d: u64,
  f: std::collections::HashMap<String, u32>,
  #[webidl(default = String::from("foo"))]
  g: String,
  #[webidl(required)]
  h: Option<bool>,
  #[options(clamp = true, enforce_range = false)]
  some_long_name: Option<i8>,
}

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Empty {}

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct WithLifetime<'s> {
  value: deno_core::v8::Local<'s, deno_core::v8::Value>,
}
