// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
#![allow(dead_code)]
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Shadowing {
  pub context: u8,
  pub scope: Option<String>,
  pub prefix: bool,
  pub value: u8,
  #[webidl(default = 1)]
  pub dictionary: u8,
}

fn main() {}
