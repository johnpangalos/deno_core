// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::WebIDL;

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(enum)]
pub enum Mode {
  FooBar,
  A,
  Http2Only,
  #[webidl(rename = "custom")]
  Renamed,
}

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Nested {
  pub inner_value: u8,
}

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dictionary {
  pub foo_bar: u8,
  #[webidl(rename = "customKey", default = 3)]
  pub renamed: u8,
  #[webidl(required)]
  pub required: String,
  #[webidl(rename = "renamedRequired", required)]
  pub renamed_required: Option<u8>,
  #[webidl(default = None)]
  pub maybe: Option<String>,
  #[options(clamp = true)]
  pub clamped: u8,
  #[options(enforce_range = true)]
  pub enforced: Option<u8>,
  #[options(clamp = true)]
  #[webidl(default = 7)]
  pub clamped_default: u8,
  pub nested: Nested,
  #[webidl(default = Mode::A)]
  pub mode: Mode,
  pub r#type: u8,
}
