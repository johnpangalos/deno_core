// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
#![allow(dead_code)]
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(enum)]
pub enum Mode {
  ReadOnly,
  #[webidl(rename = "rw")]
  ReadWrite,
}

fn main() {}
