// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();
use deno_core::WebIDL;

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(enum)]
pub enum Protocol {
  FooBar,
  A,
  Http2Only,
  #[webidl(rename = "custom-value")]
  Renamed,
}
