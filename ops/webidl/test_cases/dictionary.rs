// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
deno_ops_compile_test_runner::prelude!();

#[derive(WebIDL)]
#[webidl(dictionary)]
struct WebIdlDictionary<'a> {
  #[webidl(default = 5)]
  first_value: u8,
  #[options(enforce_range = true)]
  second_value: u16,
  #[webidl(rename = "zed")]
  renamed: Option<String>,
  #[webidl(required)]
  required: bool,
  value: v8::Local<'a, v8::Value>,
}

// Members are read in UTF-16 code unit order, so the emoji key comes first.
#[derive(WebIDL)]
#[webidl(dictionary)]
struct Utf16Keys {
  #[webidl(rename = "\u{FF61}")]
  halfwidth: u8,
  #[webidl(rename = "\u{1F600}")]
  emoji: u8,
}
