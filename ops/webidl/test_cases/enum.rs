// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

use deno_core::WebIDL;

#[allow(dead_code)]
#[derive(WebIDL)]
#[webidl(enum)]
enum Choice {
  FooBar,
  #[webidl(rename = "custom-name")]
  Other,
}
