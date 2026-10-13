// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

#[derive(WebIDL)]
#[webidl(enum)]
pub enum Method {
  FooBar,
  #[webidl(rename = "PATCH")]
  Patch,
  HTTPMethod,
}
