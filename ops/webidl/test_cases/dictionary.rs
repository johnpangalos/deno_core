// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![allow(dead_code)]
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  pub zeta: u8,
  #[webidl(rename = "customKey", default = 5)]
  pub alpha: u8,
  #[webidl(required)]
  pub foo_bar: String,
  #[options(clamp = true)]
  pub clamped: u8,
  #[options(enforce_range = true)]
  pub ranged: u16,
  pub maybe: Option<String>,
  #[webidl(default = String::new())]
  pub r#type: String,
}

pub fn main() {}
