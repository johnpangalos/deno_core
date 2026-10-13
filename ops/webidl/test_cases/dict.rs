// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#[derive(WebIDL)]
#[webidl(dictionary)]
struct Dict {
  foo_bar: u8,
  #[webidl(rename = "renamed")]
  other: String,
  #[webidl(default = 3)]
  with_default: u32,
  #[webidl(required)]
  required_value: String,
  maybe: Option<bool>,
  #[options(clamp = true)]
  clamped: Option<u8>,
  #[options(enforce_range = true)]
  listed: Vec<u8>,
  r#type: String,
}
