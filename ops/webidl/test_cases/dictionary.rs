// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::WebIDL;

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(dictionary)]
struct RequestInit {
  #[webidl(default = String::from("GET"))]
  http_method: String,
  #[webidl(required)]
  url: String,
  #[options(enforce_range = true)]
  retries: u8,
  #[webidl(rename = "custom-key", default = 0)]
  renamed: u32,
  body: Option<Vec<u8>>,
  #[webidl(default = Nested { value: 3 })]
  nested: Nested,
  #[webidl(default = String::new())]
  r#type: String,
}

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(dictionary)]
struct Nested {
  #[webidl(default = 3)]
  value: u8,
}
