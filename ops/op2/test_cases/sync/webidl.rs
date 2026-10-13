// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::cppgc::GarbageCollected;
use deno_core::v8;
use deno_core::webidl::ByteString;
use deno_core::WebIDL;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  #[webidl(required)]
  name: String,
}

#[op2]
fn op_webidl(#[webidl] s: String, #[webidl] n: u32) {
  let _ = s;
  let _ = n;
}

#[op2]
fn op_webidl_option(#[webidl] s: Option<Vec<ByteString>>) {
  let _ = s;
}

#[op2]
fn op_webidl_dict<'a>(
  #[webidl] dict: Dict,
  #[webidl] value: v8::Local<'a, v8::Value>,
) {
  let _ = dict.name;
  let _ = value;
}

pub struct Foo;

impl GarbageCollected for Foo {}

#[op2]
impl Foo {
  #[constructor]
  #[cppgc]
  pub fn constructor(#[webidl] _x: u8) -> Foo {
    Foo
  }

  pub fn bar(&self, #[webidl] _v: f64) {}
}
