// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::cppgc::GarbageCollected;
use deno_core::v8;
use std::cell::Cell;

#[derive(deno_core::WebIDL)]
#[webidl(dictionary)]
struct Dict {
  #[webidl(required)]
  foo_bar: u8,
  #[webidl(default = 5)]
  count: u32,
  other: Option<String>,
}

#[op2]
pub fn op_webidl_dict(#[webidl] dict: Dict) -> u32 {
  dict.foo_bar as u32 + dict.count
}

#[op2]
pub fn op_webidl_u8(#[webidl] value: u8) -> u32 {
  value as u32
}

#[op2]
pub fn op_webidl_any(#[webidl] value: v8::Local<v8::Value>) -> bool {
  value.is_null()
}

#[op2]
pub fn op_webidl_vec(#[webidl] v: Vec<String>) -> u32 {
  v.len() as u32
}

#[op2]
pub fn op_webidl_option(#[webidl] o: Option<String>) -> bool {
  o.is_some()
}

#[op2]
pub fn op_webidl_two_args(#[webidl] a: u8, #[webidl] b: String) -> u32 {
  a as u32 + b.len() as u32
}

pub struct WebIdlObj {
  x: Cell<u32>,
}

impl GarbageCollected for WebIdlObj {}

#[op2]
impl WebIdlObj {
  #[constructor]
  #[cppgc]
  pub fn constructor(#[webidl] x: Option<u8>) -> WebIdlObj {
    WebIdlObj {
      x: Cell::new(x.unwrap_or_default() as u32),
    }
  }

  pub fn set(&self, #[webidl] x: u8) {
    self.x.set(x as u32);
  }

  #[nofast]
  pub fn get(&self) -> u32 {
    self.x.get()
  }
}
