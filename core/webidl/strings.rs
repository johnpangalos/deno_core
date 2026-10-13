// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Converters for `DOMString`, `ByteString` and `bigint`.

use std::borrow::Cow;
use std::ops::Deref;

use super::invalid;
use super::js;
use super::WebIdlConverter;
use super::WebIdlError;
use crate::v8;

/// Options for the string converters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StringOptions {
  /// Convert `null` to the empty string instead of `"null"`.
  pub treat_null_as_empty_string: bool,
}

/// A WebIDL `ByteString`: a string whose code units are all at most 0xFF.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &String {
    &self.0
  }
}

/// A WebIDL `DOMString`. `ToString` is applied, and lone surrogates become
/// U+FFFD.
impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if options.treat_null_as_empty_string && value.is_null() {
      return Ok(String::new());
    }
    to_dom_string(scope, value, &prefix, &context)
  }
}

pub(crate) fn to_dom_string<'a>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
) -> Result<String, WebIdlError> {
  if value.is_symbol() {
    return Err(invalid(
      prefix,
      context,
      "is a Symbol and cannot be converted to a string.",
    ));
  }
  let string = js(scope, prefix, context, |tc| value.to_string(tc))?;
  Ok(string.to_rust_string_lossy(scope))
}

/// A `DOMString` whose code units must all be at most 0xFF. Each code unit maps
/// to the `char` with the same code point.
impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &StringOptions,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let string =
      String::convert(scope, value, prefix.clone(), &context, options)?;
    if string.chars().any(|c| c as u32 > 0xFF) {
      return Err(invalid(
        &prefix,
        &context,
        "contains a character with a code point greater than 255.",
      ));
    }
    Ok(ByteString(string))
  }
}

/// An arbitrary precision integer: a sign and little-endian 64-bit words. Zero
/// has no words, and a nonzero value has no high zero words.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BigInt {
  /// True when the value is negative.
  pub sign: bool,
  /// The magnitude, least significant word first.
  pub words: Vec<u64>,
}

/// `ToBigInt`. Numbers, `undefined` and `null` are errors.
impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &(),
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_symbol() {
      return Err(invalid(
        &prefix,
        &context,
        "is a Symbol and cannot be converted to a BigInt.",
      ));
    }
    let big_int = js(scope, &prefix, &context, |tc| value.to_big_int(tc))?;
    let mut buffer = vec![0u64; big_int.word_count()];
    let (sign, words) = big_int.to_words_array(&mut buffer);
    let mut words = words.to_vec();
    while words.last() == Some(&0) {
      words.pop();
    }
    Ok(BigInt {
      sign: sign && !words.is_empty(),
      words,
    })
  }
}

#[cfg(test)]
mod tests {
  use std::borrow::Cow;

  use super::*;
  use crate::webidl::with_js_value;
  use crate::webidl::WebIdlErrorKind;

  fn prefix() -> Cow<'static, str> {
    Cow::Borrowed("P")
  }

  fn context() -> Cow<'static, str> {
    Cow::Borrowed("C")
  }

  fn string(source: &str, options: StringOptions) -> Result<String, String> {
    with_js_value(source, |scope, value| {
      String::convert(scope, value, prefix(), context, &options)
        .map_err(|e| e.to_string())
    })
  }

  fn byte_string(source: &str) -> Result<String, String> {
    with_js_value(source, |scope, value| {
      ByteString::convert(
        scope,
        value,
        prefix(),
        context,
        &StringOptions::default(),
      )
      .map(|s| s.0)
      .map_err(|e| e.to_string())
    })
  }

  fn big_int(source: &str) -> Result<BigInt, String> {
    with_js_value(source, |scope, value| {
      BigInt::convert(scope, value, prefix(), context, &())
        .map_err(|e| e.to_string())
    })
  }

  fn big_int_error(source: &str) -> String {
    big_int(source).unwrap_err()
  }

  #[test]
  fn dom_string_to_string_semantics() {
    let default = StringOptions::default();
    assert_eq!(string("null", default), Ok("null".into()));
    assert_eq!(string("undefined", default), Ok("undefined".into()));
    assert_eq!(string("1.5", default), Ok("1.5".into()));
    assert_eq!(string("({})", default), Ok("[object Object]".into()));
    assert_eq!(
      string("({toString(){return 'custom'}})", default),
      Ok("custom".into())
    );
  }

  #[test]
  fn dom_string_null_option() {
    let treat = StringOptions {
      treat_null_as_empty_string: true,
    };
    assert_eq!(string("null", treat), Ok(String::new()));
    assert_eq!(string("undefined", treat), Ok("undefined".into()));
  }

  #[test]
  fn dom_string_unicode_round_trips() {
    let default = StringOptions::default();
    assert_eq!(
      string("'h\\u00e9llo \\uD83D\\uDE00'", default),
      Ok("h\u{e9}llo \u{1F600}".into())
    );
    assert_eq!(string("'\\uD800'", default), Ok("\u{FFFD}".into()));
  }

  #[test]
  fn dom_string_symbol_and_js_exception_are_errors() {
    let default = StringOptions::default();
    assert_eq!(
      string("Symbol()", default),
      Err("P: C is a Symbol and cannot be converted to a string.".into())
    );
    assert_eq!(
      string("({toString(){throw new Error('x')}})", default),
      Err("P: C threw an exception: Error: x".into())
    );
  }

  #[test]
  fn byte_string_accepts_latin1_boundary() {
    assert_eq!(byte_string("'\\u00ff'"), Ok("\u{ff}".into()));
    assert_eq!(byte_string("'\\u0000'"), Ok("\u{0}".into()));
    assert_eq!(byte_string("'\\u00e9'"), Ok("\u{e9}".into()));
  }

  #[test]
  fn byte_string_rejects_wide_characters() {
    let wide = "P: C contains a character with a code point greater than 255.";
    for source in ["'\\u0100'", "'\\u20ac'", "'\\uD83D\\uDE00'", "'\\uD800'"] {
      assert_eq!(byte_string(source), Err(wide.into()), "{source}");
    }
  }

  #[test]
  fn byte_string_js_exception_is_an_error() {
    assert_eq!(
      byte_string("({toString(){throw 'x'}})"),
      Err("P: C threw an exception: x".into())
    );
  }

  #[test]
  fn big_int_from_bigint_values() {
    assert_eq!(
      big_int("0n"),
      Ok(BigInt {
        sign: false,
        words: vec![]
      })
    );
    assert_eq!(
      big_int("-1n"),
      Ok(BigInt {
        sign: true,
        words: vec![1]
      })
    );
    assert_eq!(
      big_int("2n ** 64n"),
      Ok(BigInt {
        sign: false,
        words: vec![0, 1]
      })
    );
    assert_eq!(
      big_int("2n ** 128n"),
      Ok(BigInt {
        sign: false,
        words: vec![0, 0, 1]
      })
    );
    assert_eq!(
      big_int("-(2n ** 64n) + 1n"),
      Ok(BigInt {
        sign: true,
        words: vec![u64::MAX]
      })
    );
  }

  #[test]
  fn big_int_from_booleans_and_strings() {
    assert_eq!(big_int("true").unwrap().words, vec![1]);
    assert_eq!(big_int("false").unwrap().words, Vec::<u64>::new());
    assert_eq!(big_int("'123'").unwrap().words, vec![123]);
    assert_eq!(big_int("'  12  '").unwrap().words, vec![12]);
    assert_eq!(big_int("'0x10'").unwrap().words, vec![16]);
    assert_eq!(big_int("''").unwrap().words, Vec::<u64>::new());
  }

  #[test]
  fn big_int_rejects_non_bigint_values() {
    assert_eq!(
      big_int_error("1"),
      "P: C threw an exception: TypeError: Cannot convert 1 to a BigInt"
    );
    assert_eq!(
      big_int_error("undefined"),
      "P: C threw an exception: TypeError: Cannot convert undefined to a BigInt"
    );
    assert_eq!(
      big_int_error("null"),
      "P: C threw an exception: TypeError: Cannot convert null to a BigInt"
    );
    assert_eq!(
      big_int_error("'1.5'"),
      "P: C threw an exception: SyntaxError: Cannot convert 1.5 to a BigInt"
    );
    assert_eq!(
      big_int_error("'abc'"),
      "P: C threw an exception: SyntaxError: Cannot convert abc to a BigInt"
    );
    assert_eq!(
      big_int_error("Symbol()"),
      "P: C is a Symbol and cannot be converted to a BigInt."
    );
  }

  #[test]
  fn big_int_js_exception_is_an_error() {
    assert_eq!(
      big_int_error("({valueOf(){throw 1}})"),
      "P: C threw an exception: 1"
    );
  }

  #[test]
  fn error_kind_is_thrown_for_js_failures() {
    let err = with_js_value("({valueOf(){throw 1}})", |scope, value| {
      BigInt::convert(scope, value, prefix(), context, &()).unwrap_err()
    });
    assert_eq!(err.kind, WebIdlErrorKind::Thrown("1".into()));
  }
}
