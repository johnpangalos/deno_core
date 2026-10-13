// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::v8;
use deno_core::webidl::WebIdlConverter;
use deno_core::webidl::WebIdlError;
use std::borrow::Cow;

struct Foo;

impl<'a> WebIdlConverter<'a> for Foo {
  type Options = ();
  fn convert<C>(
    _scope: &mut v8::HandleScope<'a>,
    _value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(Foo)
  }
}

#[op2]
pub fn op_webidl_arg(#[webidl] foo: Foo) {
  let _ = foo;
}
