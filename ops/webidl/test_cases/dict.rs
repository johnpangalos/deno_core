// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
#![deny(warnings)]
#![allow(dead_code)]
use deno_core::WebIDL;
use std::collections::HashMap;

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Dict {
  pub foo_bar: u8,
  #[webidl(default = 5)]
  #[options(clamp = true)]
  pub max_value: u8,
  #[webidl(rename = "Other")]
  pub other: Option<String>,
  pub nested: Option<Vec<Inner>>,
  pub by_name: HashMap<String, Kind>,
  #[webidl(required)]
  pub required_value: bool,
}

#[derive(WebIDL)]
#[webidl(dictionary)]
pub struct Inner {
  #[options(treat_null_as_empty_string = true)]
  pub name: String,
}

#[derive(WebIDL)]
#[webidl(enum)]
pub enum Kind {
  FooBar,
  #[webidl(rename = "custom")]
  Other,
}

fn main() {}
