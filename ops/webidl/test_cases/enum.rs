// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

#[derive(WebIDL)]
#[webidl(enum)]
enum Mode {
  FooBar,
  #[webidl(rename = "HTTP")]
  Http2Request,
}
