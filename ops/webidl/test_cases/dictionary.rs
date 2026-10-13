// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

#[derive(WebIDL)]
#[webidl(dictionary)]
struct Dictionary {
  #[webidl(required)]
  foo_bar: u8,
  #[webidl(default = 3)]
  #[options(clamp = true)]
  count: u8,
  maybe: Option<String>,
  #[webidl(rename = "renamed")]
  other: String,
}

#[derive(WebIDL)]
#[webidl(dictionary)]
struct Empty {}
