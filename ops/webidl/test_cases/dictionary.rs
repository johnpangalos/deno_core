// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct RenameAndDefault {
  #[webidl(rename = "renamed")]
  foo: u8,
  #[webidl(default = 3)]
  bar_baz: u8,
}

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct RequiredAndOptions {
  #[webidl(required)]
  foo_bar: String,
  maybe: Option<String>,
  #[options(clamp = true)]
  clamped: Option<u8>,
  #[options(enforce_range = true)]
  ranged: Vec<u16>,
}
