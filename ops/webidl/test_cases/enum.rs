// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![allow(dead_code)]
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(enum)]
pub enum Choice {
  FooBar,
  #[webidl(rename = "null")]
  Nothing,
  A,
}

pub fn main() {}
