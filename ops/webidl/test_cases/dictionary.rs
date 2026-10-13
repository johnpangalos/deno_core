// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Options<'a> {
  #[webidl(default = 1)]
  retry_count: u8,
  #[options(clamp = true)]
  timeout: Option<u8>,
  #[webidl(rename = "mode", required)]
  kind: String,
  #[options(treat_null_as_empty_string = true)]
  label: String,
  value: v8::Local<'a, v8::Value>,
}
