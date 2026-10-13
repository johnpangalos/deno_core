// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

#[derive(WebIDL)]
#[webidl(dictionary)]
struct Dict {
  #[webidl(required)]
  foo_bar: u8,
  #[webidl(default = 5)]
  count: u32,
  #[webidl(rename = "renamed")]
  other: Option<String>,
  #[options(clamp = true, enforce_range = false)]
  #[webidl(default = 0)]
  clamped: u8,
  r#type: Option<String>,
}
