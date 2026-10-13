// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Conversion of JavaScript values to WebIDL types, following the
//! [JavaScript type mapping](https://webidl.spec.whatwg.org/#js-type-mapping)
//! algorithms of the WebIDL specification.
//!
//! Dictionaries and enums can be implemented with `#[derive(WebIDL)]`, and op
//! arguments can be converted using the `#[webidl]` attribute in `#[op2]`.

// Prefixes are passed around as `&Cow` so that they can be cheaply cloned into
// errors.
#![allow(clippy::ptr_arg)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::fmt::Formatter;
use std::ops::Deref;

/// An error that occurred while converting a JavaScript value to a WebIDL
/// type.
#[derive(Debug)]
pub struct WebIdlError {
  /// Where the conversion was initiated (ie: "Failed to execute 'fetch'").
  pub prefix: Cow<'static, str>,
  /// The value being converted (ie: "Argument 1").
  pub context: Cow<'static, str>,
  /// What went wrong.
  pub kind: WebIdlErrorKind,
}

impl WebIdlError {
  pub fn new<C>(
    prefix: Cow<'static, str>,
    context: C,
    kind: WebIdlErrorKind,
  ) -> Self
  where
    C: Fn() -> Cow<'static, str>,
  {
    Self {
      prefix,
      context: context(),
      kind,
    }
  }
}

impl Display for WebIdlError {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}: {} {}", self.prefix, self.context, self.kind)
  }
}

impl std::error::Error for WebIdlError {}

#[derive(Debug)]
pub enum WebIdlErrorKind {
  /// The value is not of a type that can be converted to the given type.
  ConvertToConverterType(&'static str),
  /// The value is not a finite number.
  NotFinite,
  /// The value is outside of the range of an `[EnforceRange]` integer.
  IntRange {
    lower_bound: f64,
    upper_bound: f64,
  },
  /// The value contains code units above 255.
  InvalidByteString,
  /// A required dictionary member is missing.
  DictionaryCannotConvertKey {
    converter: &'static str,
    key: &'static str,
  },
  /// The value is not one of the values of an enumeration.
  InvalidEnumVariant {
    converter: &'static str,
    variant: String,
  },
  /// An exception was thrown by JavaScript code during the conversion.
  Exception(String),
  Other(Cow<'static, str>),
}

impl Display for WebIdlErrorKind {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::ConvertToConverterType(ty) => {
        write!(f, "can not be converted to {ty}")
      }
      Self::NotFinite => write!(f, "is not a finite number"),
      Self::IntRange {
        lower_bound,
        upper_bound,
      } => write!(
        f,
        "is outside the accepted range of {lower_bound} to {upper_bound}, inclusive"
      ),
      Self::InvalidByteString => write!(f, "is not a valid ByteString"),
      Self::DictionaryCannotConvertKey { converter, key } => write!(
        f,
        "can not be converted to '{converter}' because '{key}' is required in '{converter}'"
      ),
      Self::InvalidEnumVariant { converter, variant } => write!(
        f,
        "with value '{variant}' is not a valid enum value of type '{converter}'"
      ),
      Self::Exception(message) => {
        write!(f, "could not be converted: {message}")
      }
      Self::Other(message) => write!(f, "{message}"),
    }
  }
}

/// Converts a JavaScript value to a WebIDL type.
pub trait WebIdlConverter<'a>: Sized {
  /// Extended attributes that change how the value is converted, such as
  /// `[Clamp]`.
  type Options: Default;

  /// Convert `value`. `prefix` and `context` describe where the value came
  /// from, and are used for error messages.
  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>;
}

/// Run `f`, turning any JavaScript exception thrown by it into a
/// [`WebIdlError`].
fn try_js<'a, T, C>(
  scope: &mut v8::HandleScope<'a>,
  prefix: &Cow<'static, str>,
  context: &C,
  f: impl FnOnce(&mut v8::HandleScope<'a>) -> Option<T>,
) -> Result<T, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  let tc = &mut v8::TryCatch::new(scope);
  if let Some(value) = f(tc) {
    return Ok(value);
  }
  let message = match tc.exception() {
    Some(exception) => exception.to_rust_string_lossy(tc),
    None => "execution terminated".to_string(),
  };
  Err(WebIdlError::new(
    prefix.clone(),
    context,
    WebIdlErrorKind::Exception(message),
  ))
}

fn to_number<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: &C,
) -> Result<f64, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if let Ok(number) = value.try_cast::<v8::Number>() {
    return Ok(number.value());
  }
  try_js(scope, prefix, context, |scope| {
    value.to_number(scope).map(|n| n.value())
  })
}

fn to_string<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: &C,
) -> Result<v8::Local<'a, v8::String>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  if let Ok(string) = value.try_cast::<v8::String>() {
    return Ok(string);
  }
  try_js(scope, prefix, context, |scope| value.to_string(scope))
}

fn get<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: v8::Local<'a, v8::Value>,
  prefix: &Cow<'static, str>,
  context: &C,
) -> Result<v8::Local<'a, v8::Value>, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  try_js(scope, prefix, context, |scope| object.get(scope, key))
}

// any
impl<'a> WebIdlConverter<'a> for v8::Local<'a, v8::Value> {
  type Options = ();

  fn convert<C>(
    _scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value)
  }
}

// boolean
impl<'a> WebIdlConverter<'a> for bool {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    _prefix: Cow<'static, str>,
    _context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    Ok(value.boolean_value(scope))
  }
}

/// Extended attributes for integer types.
#[derive(Debug, Default, Clone, Copy)]
pub struct IntOptions {
  /// `[Clamp]`
  pub clamp: bool,
  /// `[EnforceRange]`
  pub enforce_range: bool,
}

enum IntValue {
  /// An integral value within the bounds of the type.
  InRange(f64),
  /// The value modulo 2^64; truncating it gives the value modulo 2^bits.
  Wrapped(u64),
}

/// https://webidl.spec.whatwg.org/#abstract-opdef-converttoint
fn convert_to_int<'a, C>(
  scope: &mut v8::HandleScope<'a>,
  value: v8::Local<'a, v8::Value>,
  prefix: Cow<'static, str>,
  context: C,
  options: &IntOptions,
  bit_length: u32,
  signed: bool,
) -> Result<IntValue, WebIdlError>
where
  C: Fn() -> Cow<'static, str>,
{
  const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

  let (lower_bound, upper_bound) = match (bit_length, signed) {
    (64, true) => (-MAX_SAFE_INTEGER, MAX_SAFE_INTEGER),
    (64, false) => (0.0, MAX_SAFE_INTEGER),
    (bits, true) => (
      -(2f64.powi(bits as i32 - 1)),
      2f64.powi(bits as i32 - 1) - 1.0,
    ),
    (bits, false) => (0.0, 2f64.powi(bits as i32) - 1.0),
  };

  let x = to_number(scope, value, &prefix, &context)?;

  if options.enforce_range {
    if !x.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    let x = x.trunc();
    if x < lower_bound || x > upper_bound {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::IntRange {
          lower_bound,
          upper_bound,
        },
      ));
    }
    // Normalize -0 to +0
    return Ok(IntValue::InRange(x + 0.0));
  }

  if !x.is_nan() && options.clamp {
    let x = x.clamp(lower_bound, upper_bound).round_ties_even();
    return Ok(IntValue::InRange(x + 0.0));
  }

  if !x.is_finite() || x == 0.0 {
    return Ok(IntValue::InRange(0.0));
  }

  // Both the truncation and the remainder are exact.
  let x = x.trunc() % 18446744073709551616.0;
  let wrapped = if x >= 0.0 {
    x as u64
  } else {
    0u64.wrapping_sub((-x) as u64)
  };
  Ok(IntValue::Wrapped(wrapped))
}

macro_rules! impl_ints {
  ($($t:ty: $bits:literal, $signed:literal;)*) => {
    $(
      impl<'a> WebIdlConverter<'a> for $t {
        type Options = IntOptions;

        fn convert<C>(
          scope: &mut v8::HandleScope<'a>,
          value: v8::Local<'a, v8::Value>,
          prefix: Cow<'static, str>,
          context: C,
          options: &Self::Options,
        ) -> Result<Self, WebIdlError>
        where
          C: Fn() -> Cow<'static, str>,
        {
          let value = convert_to_int(
            scope, value, prefix, context, options, $bits, $signed,
          )?;
          Ok(match value {
            IntValue::InRange(x) => x as $t,
            IntValue::Wrapped(x) => x as $t,
          })
        }
      }
    )*
  };
}

impl_ints! {
  i8: 8, true;
  u8: 8, false;
  i16: 16, true;
  u16: 16, false;
  i32: 32, true;
  u32: 32, false;
  i64: 64, true;
  u64: 64, false;
}

// float
impl<'a> WebIdlConverter<'a> for f32 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    // Rounds to the nearest value, ties to even, and to infinity for values
    // that round to 2^128 or beyond.
    let y = x as f32;
    if !y.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    Ok(y)
  }
}

/// `unrestricted float`
#[derive(Debug, Default, Clone, Copy, PartialEq, PartialOrd)]
pub struct UnrestrictedFloat(pub f32);

impl Deref for UnrestrictedFloat {
  type Target = f32;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedFloat {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    Ok(UnrestrictedFloat(x as f32))
  }
}

// double
impl<'a> WebIdlConverter<'a> for f64 {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let x = to_number(scope, value, &prefix, &context)?;
    if !x.is_finite() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::NotFinite,
      ));
    }
    Ok(x)
  }
}

/// `unrestricted double`
#[derive(Debug, Default, Clone, Copy, PartialEq, PartialOrd)]
pub struct UnrestrictedDouble(pub f64);

impl Deref for UnrestrictedDouble {
  type Target = f64;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<'a> WebIdlConverter<'a> for UnrestrictedDouble {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    to_number(scope, value, &prefix, &context).map(UnrestrictedDouble)
  }
}

/// `bigint`
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BigInt {
  /// Whether the value is negative.
  pub sign: bool,
  /// The magnitude of the value, as 64-bit words, least significant first.
  pub words: Vec<u64>,
}

impl<'a> WebIdlConverter<'a> for BigInt {
  type Options = ();

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    _options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let bigint = match value.try_cast::<v8::BigInt>() {
      Ok(bigint) => bigint,
      Err(_) => {
        try_js(scope, &prefix, &context, |scope| value.to_big_int(scope))?
      }
    };
    let mut words = vec![0; bigint.word_count()];
    let (sign, _) = bigint.to_words_array(&mut words);
    Ok(BigInt { sign, words })
  }
}

/// Extended attributes for string types.
#[derive(Debug, Default, Clone, Copy)]
pub struct StringOptions {
  /// `[LegacyNullToEmptyString]`
  pub treat_null_as_empty_string: bool,
}

// DOMString
impl<'a> WebIdlConverter<'a> for String {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_null() && options.treat_null_as_empty_string {
      return Ok(String::new());
    }
    let string = to_string(scope, value, &prefix, &context)?;
    Ok(string.to_rust_string_lossy(scope))
  }
}

/// `ByteString`: a string whose characters are all in the range U+0000 to
/// U+00FF.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ByteString(pub String);

impl Deref for ByteString {
  type Target = String;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<'a> WebIdlConverter<'a> for ByteString {
  type Options = StringOptions;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_null() && options.treat_null_as_empty_string {
      return Ok(ByteString(String::new()));
    }
    let string = to_string(scope, value, &prefix, &context)?;
    if !string.contains_only_onebyte() {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::InvalidByteString,
      ));
    }
    Ok(ByteString(string.to_rust_string_lossy(scope)))
  }
}

// nullable
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Option<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_null_or_undefined() {
      Ok(None)
    } else {
      T::convert(scope, value, prefix, context, options).map(Some)
    }
  }
}

// sequence<T>
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Vec<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let not_sequence = |prefix, context| {
      WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::ConvertToConverterType("sequence"),
      )
    };

    let Ok(object) = value.try_cast::<v8::Object>() else {
      return Err(not_sequence(prefix, &context));
    };

    // GetMethod(V, @@iterator)
    let iterator_key = v8::Symbol::get_iterator(scope);
    let method = get(scope, object, iterator_key.into(), &prefix, &context)?;
    let Ok(method) = method.try_cast::<v8::Function>() else {
      return Err(not_sequence(prefix, &context));
    };

    // GetIteratorFromMethod(V, method)
    let iterator = try_js(scope, &prefix, &context, |scope| {
      method.call(scope, object.into(), &[])
    })?;
    let Ok(iterator) = iterator.try_cast::<v8::Object>() else {
      return Err(not_sequence(prefix, &context));
    };
    let next_key = v8::String::new(scope, "next").unwrap();
    let next = get(scope, iterator, next_key.into(), &prefix, &context)?;
    let Ok(next) = next.try_cast::<v8::Function>() else {
      return Err(not_sequence(prefix, &context));
    };

    let done_key = v8::String::new(scope, "done").unwrap();
    let value_key = v8::String::new(scope, "value").unwrap();

    let mut out = vec![];
    loop {
      // IteratorStepValue(iteratorRecord)
      let result = try_js(scope, &prefix, &context, |scope| {
        next.call(scope, iterator.into(), &[])
      })?;
      let Ok(result) = result.try_cast::<v8::Object>() else {
        return Err(WebIdlError::new(
          prefix,
          context,
          WebIdlErrorKind::Other(
            "can not be converted to sequence because its iterator result is not an object"
              .into(),
          ),
        ));
      };
      let done = get(scope, result, done_key.into(), &prefix, &context)?;
      if done.boolean_value(scope) {
        break;
      }
      let item = get(scope, result, value_key.into(), &prefix, &context)?;

      let index = out.len();
      let item = T::convert(
        scope,
        item,
        prefix.clone(),
        || format!("{}, index {index}", context()).into(),
        options,
      )?;
      out.push(item);
    }

    Ok(out)
  }
}

// record<DOMString, T>
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for HashMap<String, T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &Self::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Ok(object) = value.try_cast::<v8::Object>() else {
      return Err(WebIdlError::new(
        prefix,
        context,
        WebIdlErrorKind::ConvertToConverterType("record"),
      ));
    };

    // O.[[OwnPropertyKeys]]()
    let keys = try_js(scope, &prefix, &context, |scope| {
      object.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
          mode: v8::KeyCollectionMode::OwnOnly,
          property_filter: v8::PropertyFilter::ALL_PROPERTIES,
          index_filter: v8::IndexFilter::IncludeIndices,
          key_conversion: v8::KeyConversionMode::ConvertToString,
        },
      )
    })?;

    let enumerable_key = v8::String::new(scope, "enumerable").unwrap();

    let mut out = HashMap::with_capacity(keys.length() as usize);
    for i in 0..keys.length() {
      let key =
        try_js(scope, &prefix, &context, |scope| keys.get_index(scope, i))?;
      let Ok(name) = key.try_cast::<v8::Name>() else {
        continue;
      };

      // O.[[GetOwnProperty]](key)
      let descriptor = try_js(scope, &prefix, &context, |scope| {
        object.get_own_property_descriptor(scope, name)
      })?;
      let Ok(descriptor) = descriptor.try_cast::<v8::Object>() else {
        continue;
      };
      let enumerable =
        get(scope, descriptor, enumerable_key.into(), &prefix, &context)?;
      if !enumerable.boolean_value(scope) {
        continue;
      }

      let typed_key = String::convert(
        scope,
        key,
        prefix.clone(),
        || format!("{}'s key", context()).into(),
        &Default::default(),
      )?;
      let item = get(scope, object, key, &prefix, &context)?;
      let item = T::convert(
        scope,
        item,
        prefix.clone(),
        || format!("{}'s value for key '{typed_key}'", context()).into(),
        options,
      )?;
      out.insert(typed_key, item);
    }

    Ok(out)
  }
}

/// Helpers used by the code generated by `#[derive(WebIDL)]`.
#[doc(hidden)]
pub mod __private {
  use super::*;

  /// Steps 1 and 2 of converting a value to a dictionary: the value must be
  /// undefined, null or an object. Undefined and null are treated as an empty
  /// dictionary.
  pub fn dictionary_object<'a, C>(
    value: v8::Local<'a, v8::Value>,
    prefix: &Cow<'static, str>,
    context: &C,
  ) -> Result<Option<v8::Local<'a, v8::Object>>, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    if value.is_null_or_undefined() {
      return Ok(None);
    }
    match value.try_cast::<v8::Object>() {
      Ok(object) => Ok(Some(object)),
      Err(_) => Err(WebIdlError::new(
        prefix.clone(),
        context,
        WebIdlErrorKind::ConvertToConverterType("dictionary"),
      )),
    }
  }

  /// Get a dictionary member, returning `None` if it is undefined.
  pub fn dictionary_member<'a, C>(
    scope: &mut v8::HandleScope<'a>,
    object: Option<v8::Local<'a, v8::Object>>,
    key: &'static str,
    prefix: &Cow<'static, str>,
    context: &C,
  ) -> Result<Option<v8::Local<'a, v8::Value>>, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Some(object) = object else {
      return Ok(None);
    };
    let key = v8::String::new(scope, key).unwrap();
    let value = get(scope, object, key.into(), prefix, context)?;
    if value.is_undefined() {
      Ok(None)
    } else {
      Ok(Some(value))
    }
  }

  /// The context for a dictionary member, ie: "'key' of 'Dictionary' (Argument 1)".
  pub fn dictionary_member_context<C>(
    converter: &'static str,
    key: &'static str,
    context: &C,
  ) -> Cow<'static, str>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let context = context();
    if context.is_empty() {
      format!("'{key}' of '{converter}'").into()
    } else {
      format!("'{key}' of '{converter}' ({context})").into()
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::JsRuntime;

  fn eval<'a>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> v8::Local<'a, v8::Value> {
    let code = v8::String::new(scope, code).unwrap();
    let script = v8::Script::compile(scope, code, None).unwrap();
    script.run(scope).unwrap()
  }

  fn convert<'a, T: WebIdlConverter<'a>>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
    options: &T::Options,
  ) -> Result<T, WebIdlError> {
    let value = eval(scope, code);
    T::convert(scope, value, "prefix".into(), || "context".into(), options)
  }

  fn ok<'a, T: WebIdlConverter<'a>>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> T {
    convert::<T>(scope, code, &Default::default())
      .unwrap_or_else(|err| panic!("{code}: {err}"))
  }

  fn err<'a, T: WebIdlConverter<'a> + std::fmt::Debug>(
    scope: &mut v8::HandleScope<'a>,
    code: &str,
  ) -> String {
    convert::<T>(scope, code, &Default::default())
      .unwrap_err()
      .to_string()
  }

  #[test]
  fn any() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let value = ok::<v8::Local<v8::Value>>(scope, "'foo'");
    assert!(value.is_string());
    let value = ok::<v8::Local<v8::Value>>(scope, "undefined");
    assert!(value.is_undefined());
  }

  #[test]
  fn boolean() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    for (code, expected) in [
      ("true", true),
      ("false", false),
      ("undefined", false),
      ("null", false),
      ("0", false),
      ("NaN", false),
      ("''", false),
      ("0n", false),
      ("1", true),
      ("'false'", true),
      ("({})", true),
      ("Symbol()", true),
    ] {
      assert_eq!(ok::<bool>(scope, code), expected, "{code}");
    }
  }

  #[test]
  fn integers() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<u8>(scope, "1"), 1);
    assert_eq!(ok::<u8>(scope, "1.9"), 1);
    assert_eq!(ok::<u8>(scope, "-1"), 255);
    assert_eq!(ok::<u8>(scope, "256"), 0);
    assert_eq!(ok::<u8>(scope, "257.5"), 1);
    assert_eq!(ok::<u8>(scope, "NaN"), 0);
    assert_eq!(ok::<u8>(scope, "Infinity"), 0);
    assert_eq!(ok::<u8>(scope, "-Infinity"), 0);
    assert_eq!(ok::<u8>(scope, "'12'"), 12);
    assert_eq!(ok::<u8>(scope, "'foo'"), 0);
    assert_eq!(ok::<u8>(scope, "undefined"), 0);
    assert_eq!(ok::<u8>(scope, "null"), 0);
    assert_eq!(ok::<u8>(scope, "true"), 1);
    assert_eq!(ok::<u8>(scope, "({ valueOf() { return 7 } })"), 7);

    assert_eq!(ok::<i8>(scope, "127"), 127);
    assert_eq!(ok::<i8>(scope, "128"), -128);
    assert_eq!(ok::<i8>(scope, "-129"), 127);
    assert_eq!(ok::<i8>(scope, "-1.9"), -1);
    assert_eq!(ok::<i8>(scope, "255"), -1);

    assert_eq!(ok::<u16>(scope, "65537"), 1);
    assert_eq!(ok::<u16>(scope, "-1"), 65535);
    assert_eq!(ok::<i16>(scope, "32768"), -32768);
    assert_eq!(ok::<i16>(scope, "-32769"), 32767);

    assert_eq!(ok::<u32>(scope, "4294967296"), 0);
    assert_eq!(ok::<u32>(scope, "-1"), u32::MAX);
    assert_eq!(ok::<u32>(scope, "4294967295"), u32::MAX);
    assert_eq!(ok::<i32>(scope, "2147483648"), i32::MIN);
    assert_eq!(ok::<i32>(scope, "-2147483649"), i32::MAX);
    assert_eq!(ok::<i32>(scope, "1e20"), 1661992960);

    assert_eq!(ok::<u64>(scope, "-1"), u64::MAX);
    assert_eq!(ok::<u64>(scope, "2 ** 64"), 0);
    assert_eq!(ok::<u64>(scope, "2 ** 64 + 2 ** 12"), 1 << 12);
    assert_eq!(ok::<u64>(scope, "2 ** 65 + 2 ** 63"), 1 << 63);
    assert_eq!(ok::<u64>(scope, "-(2 ** 63)"), 1 << 63);
    assert_eq!(ok::<u64>(scope, "Number.MAX_SAFE_INTEGER"), (1 << 53) - 1);
    assert_eq!(ok::<i64>(scope, "-1"), -1);
    assert_eq!(ok::<i64>(scope, "2 ** 63"), i64::MIN);
    assert_eq!(ok::<i64>(scope, "-(2 ** 63)"), i64::MIN);
    assert_eq!(ok::<i64>(scope, "2 ** 64 - 2 ** 11"), -(1 << 11));
    assert_eq!(
      ok::<i64>(scope, "-Number.MAX_SAFE_INTEGER"),
      -((1 << 53) - 1)
    );
    assert_eq!(ok::<i64>(scope, "1e300"), 0);

    assert_eq!(
      err::<u8>(scope, "1n"),
      "prefix: context could not be converted: TypeError: Cannot convert a BigInt value to a number"
    );
    assert_eq!(
      err::<u8>(scope, "Symbol()"),
      "prefix: context could not be converted: TypeError: Cannot convert a Symbol value to a number"
    );
    assert_eq!(
      err::<u8>(scope, "({ valueOf() { throw new Error('boom') } })"),
      "prefix: context could not be converted: Error: boom"
    );
  }

  #[test]
  fn integers_enforce_range() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      clamp: false,
      enforce_range: true,
    };

    assert_eq!(convert::<u8>(scope, "255", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "255.9", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-0.9", &options).unwrap(), 0);
    assert_eq!(convert::<i8>(scope, "-128", &options).unwrap(), -128);
    assert_eq!(
      convert::<u64>(scope, "Number.MAX_SAFE_INTEGER", &options).unwrap(),
      (1 << 53) - 1
    );
    assert_eq!(
      convert::<i64>(scope, "-Number.MAX_SAFE_INTEGER", &options).unwrap(),
      -((1 << 53) - 1)
    );

    assert_eq!(
      convert::<u8>(scope, "256", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of 0 to 255, inclusive"
    );
    assert_eq!(
      convert::<i8>(scope, "-129", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of -128 to 127, inclusive"
    );
    assert_eq!(
      convert::<u32>(scope, "-1", &options).unwrap_err().to_string(),
      "prefix: context is outside the accepted range of 0 to 4294967295, inclusive"
    );
    assert_eq!(
      convert::<u64>(scope, "2 ** 53", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of 0 to 9007199254740991, inclusive"
    );
    assert_eq!(
      convert::<i64>(scope, "-(2 ** 53)", &options)
        .unwrap_err()
        .to_string(),
      "prefix: context is outside the accepted range of -9007199254740991 to 9007199254740991, inclusive"
    );
    for code in ["NaN", "Infinity", "-Infinity", "'foo'", "undefined"] {
      assert_eq!(
        convert::<i32>(scope, code, &options)
          .unwrap_err()
          .to_string(),
        "prefix: context is not a finite number",
        "{code}"
      );
    }
  }

  #[test]
  fn integers_clamp() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let options = IntOptions {
      clamp: true,
      enforce_range: false,
    };

    assert_eq!(convert::<u8>(scope, "256", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-1", &options).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "Infinity", &options).unwrap(), 255);
    assert_eq!(convert::<u8>(scope, "-Infinity", &options).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "NaN", &options).unwrap(), 0);
    // Rounds half to even
    assert_eq!(convert::<u8>(scope, "0.5", &options).unwrap(), 0);
    assert_eq!(convert::<u8>(scope, "1.5", &options).unwrap(), 2);
    assert_eq!(convert::<u8>(scope, "2.5", &options).unwrap(), 2);
    assert_eq!(convert::<u8>(scope, "2.6", &options).unwrap(), 3);
    assert_eq!(convert::<i8>(scope, "-2.5", &options).unwrap(), -2);
    assert_eq!(convert::<i8>(scope, "-200", &options).unwrap(), -128);
    assert_eq!(convert::<i8>(scope, "200", &options).unwrap(), 127);
    assert_eq!(convert::<i32>(scope, "1e20", &options).unwrap(), i32::MAX);
    assert_eq!(
      convert::<u64>(scope, "2 ** 64", &options).unwrap(),
      (1 << 53) - 1
    );
    assert_eq!(
      convert::<i64>(scope, "-(2 ** 64)", &options).unwrap(),
      -((1 << 53) - 1)
    );
  }

  #[test]
  fn floats() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<f64>(scope, "1.5"), 1.5);
    assert_eq!(ok::<f64>(scope, "'2.5'"), 2.5);
    assert_eq!(ok::<f64>(scope, "null"), 0.0);
    assert!(ok::<f64>(scope, "-0").is_sign_negative());
    assert_eq!(ok::<f64>(scope, "Number.MAX_VALUE"), f64::MAX);
    for code in ["NaN", "Infinity", "-Infinity", "undefined", "'foo'"] {
      assert_eq!(
        err::<f64>(scope, code),
        "prefix: context is not a finite number"
      );
      assert_eq!(
        err::<f32>(scope, code),
        "prefix: context is not a finite number"
      );
    }

    assert_eq!(ok::<f32>(scope, "1.5"), 1.5);
    assert_eq!(ok::<f32>(scope, "0.1"), 0.1f32);
    assert!(ok::<f32>(scope, "-0").is_sign_negative());
    assert_eq!(ok::<f32>(scope, "3.4028234663852886e38"), f32::MAX);
    assert_eq!(
      err::<f32>(scope, "Number.MAX_VALUE"),
      "prefix: context is not a finite number"
    );
    assert_eq!(
      err::<f32>(scope, "2 ** 128"),
      "prefix: context is not a finite number"
    );

    assert_eq!(*ok::<UnrestrictedDouble>(scope, "1.5"), 1.5);
    assert!(ok::<UnrestrictedDouble>(scope, "NaN").is_nan());
    assert_eq!(*ok::<UnrestrictedDouble>(scope, "Infinity"), f64::INFINITY);
    assert_eq!(
      *ok::<UnrestrictedDouble>(scope, "-Infinity"),
      f64::NEG_INFINITY
    );

    assert_eq!(*ok::<UnrestrictedFloat>(scope, "1.5"), 1.5);
    assert!(ok::<UnrestrictedFloat>(scope, "NaN").is_nan());
    assert_eq!(*ok::<UnrestrictedFloat>(scope, "Infinity"), f32::INFINITY);
    assert_eq!(
      *ok::<UnrestrictedFloat>(scope, "Number.MAX_VALUE"),
      f32::INFINITY
    );
    assert_eq!(
      *ok::<UnrestrictedFloat>(scope, "-Number.MAX_VALUE"),
      f32::NEG_INFINITY
    );

    assert_eq!(
      err::<f64>(scope, "1n"),
      "prefix: context could not be converted: TypeError: Cannot convert a BigInt value to a number"
    );
  }

  #[test]
  fn bigint() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(
      ok::<BigInt>(scope, "0n"),
      BigInt {
        sign: false,
        words: vec![]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "123n"),
      BigInt {
        sign: false,
        words: vec![123]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "-(2n ** 64n + 5n)"),
      BigInt {
        sign: true,
        words: vec![5, 1]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "true"),
      BigInt {
        sign: false,
        words: vec![1]
      }
    );
    assert_eq!(
      ok::<BigInt>(scope, "'42'"),
      BigInt {
        sign: false,
        words: vec![42]
      }
    );
    assert_eq!(
      err::<BigInt>(scope, "1"),
      "prefix: context could not be converted: TypeError: Cannot convert 1 to a BigInt"
    );
    assert_eq!(
      err::<BigInt>(scope, "'foo'"),
      "prefix: context could not be converted: SyntaxError: Cannot convert foo to a BigInt"
    );
    assert!(err::<BigInt>(scope, "undefined").contains("TypeError"));
  }

  #[test]
  fn dom_string() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<String>(scope, "'foo'"), "foo");
    assert_eq!(ok::<String>(scope, "'héllo 🦕'"), "héllo 🦕");
    assert_eq!(ok::<String>(scope, "1.5"), "1.5");
    assert_eq!(ok::<String>(scope, "null"), "null");
    assert_eq!(ok::<String>(scope, "undefined"), "undefined");
    assert_eq!(ok::<String>(scope, "({})"), "[object Object]");
    assert_eq!(ok::<String>(scope, "10n"), "10");
    assert_eq!(
      convert::<String>(
        scope,
        "null",
        &StringOptions {
          treat_null_as_empty_string: true
        }
      )
      .unwrap(),
      ""
    );
    assert_eq!(
      convert::<String>(
        scope,
        "undefined",
        &StringOptions {
          treat_null_as_empty_string: true
        }
      )
      .unwrap(),
      "undefined"
    );
    assert_eq!(
      err::<String>(scope, "Symbol('foo')"),
      "prefix: context could not be converted: TypeError: Cannot convert a Symbol value to a string"
    );
  }

  #[test]
  fn byte_string() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(*ok::<ByteString>(scope, "'foo'"), "foo");
    assert_eq!(*ok::<ByteString>(scope, "'\\xff\\x00'"), "\u{ff}\u{0}");
    assert_eq!(*ok::<ByteString>(scope, "1"), "1");
    assert_eq!(*ok::<ByteString>(scope, "null"), "null");
    assert_eq!(
      *convert::<ByteString>(
        scope,
        "null",
        &StringOptions {
          treat_null_as_empty_string: true
        }
      )
      .unwrap(),
      ""
    );
    assert_eq!(
      err::<ByteString>(scope, "'\\u0100'"),
      "prefix: context is not a valid ByteString"
    );
    assert_eq!(
      err::<ByteString>(scope, "'🦕'"),
      "prefix: context is not a valid ByteString"
    );
  }

  #[test]
  fn nullable() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<Option<u8>>(scope, "null"), None);
    assert_eq!(ok::<Option<u8>>(scope, "undefined"), None);
    assert_eq!(ok::<Option<u8>>(scope, "3"), Some(3));
    assert_eq!(ok::<Option<String>>(scope, "'a'"), Some("a".to_string()));
    assert_eq!(
      convert::<Option<u8>>(
        scope,
        "300",
        &IntOptions {
          clamp: true,
          enforce_range: false
        }
      )
      .unwrap(),
      Some(255)
    );
    assert_eq!(
      err::<Option<f64>>(scope, "NaN"),
      "prefix: context is not a finite number"
    );
  }

  #[test]
  fn sequence() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<Vec<u8>>(scope, "[]"), Vec::<u8>::new());
    assert_eq!(ok::<Vec<u8>>(scope, "[1, 2, 256]"), vec![1, 2, 0]);
    assert_eq!(
      ok::<Vec<String>>(scope, "new Set(['a', 'b'])"),
      vec!["a", "b"]
    );
    assert_eq!(
      ok::<Vec<u32>>(scope, "(function* () { yield 1; yield 2; })()"),
      vec![1, 2]
    );
    assert_eq!(
      ok::<Vec<Vec<u8>>>(scope, "[[1], [2, 3]]"),
      vec![vec![1], vec![2, 3]]
    );
    assert_eq!(
      ok::<Vec<u8>>(
        scope,
        "({ [Symbol.iterator]() { let i = 0; return { next: () => ({ done: i == 3, value: i++ }) } } })"
      ),
      vec![0, 1, 2]
    );
    assert_eq!(
      convert::<Vec<u8>>(
        scope,
        "[1000]",
        &IntOptions {
          clamp: true,
          enforce_range: false
        }
      )
      .unwrap(),
      vec![255]
    );

    for code in ["1", "'abc'", "({})", "null", "undefined"] {
      assert_eq!(
        err::<Vec<u8>>(scope, code),
        "prefix: context can not be converted to sequence",
        "{code}"
      );
    }
    assert_eq!(
      err::<Vec<f64>>(scope, "[1, NaN]"),
      "prefix: context, index 1 is not a finite number"
    );
    assert_eq!(
      err::<Vec<Vec<f64>>>(scope, "[[], [1, 2, Infinity]]"),
      "prefix: context, index 1, index 2 is not a finite number"
    );
    assert_eq!(
      err::<Vec<u8>>(
        scope,
        "(function* () { yield 1; throw new Error('boom'); })()"
      ),
      "prefix: context could not be converted: Error: boom"
    );
  }

  #[test]
  fn record() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<HashMap<String, u8>>(scope, "({})"), HashMap::new());
    assert_eq!(
      ok::<HashMap<String, u8>>(scope, "({ a: 1, b: '2', 3: 3 })"),
      HashMap::from([
        ("a".to_string(), 1),
        ("b".to_string(), 2),
        ("3".to_string(), 3)
      ])
    );
    assert_eq!(
      ok::<HashMap<String, String>>(scope, "['x', 'y']"),
      HashMap::from([
        ("0".to_string(), "x".to_string()),
        ("1".to_string(), "y".to_string())
      ])
    );
    // Non-enumerable and inherited properties are skipped
    assert_eq!(
      ok::<HashMap<String, u8>>(
        scope,
        "Object.defineProperty(Object.create({ a: 1 }), 'b', { value: 2, enumerable: false })"
      ),
      HashMap::new()
    );
    // Non-enumerable symbols are skipped
    assert_eq!(
      ok::<HashMap<String, u8>>(
        scope,
        "Object.defineProperty({ a: 1 }, Symbol(), { value: 2, enumerable: false })"
      ),
      HashMap::from([("a".to_string(), 1)])
    );
    assert_eq!(
      err::<HashMap<String, u8>>(scope, "({ [Symbol('a')]: 1 })"),
      "prefix: context's key could not be converted: TypeError: Cannot convert a Symbol value to a string"
    );
    assert_eq!(
      err::<HashMap<String, f64>>(scope, "({ a: 1, b: NaN })"),
      "prefix: context's value for key 'b' is not a finite number"
    );
    assert_eq!(
      err::<HashMap<String, u8>>(
        scope,
        "({ get a() { throw new Error('boom') } })"
      ),
      "prefix: context could not be converted: Error: boom"
    );
    for code in ["1", "'abc'", "null", "undefined"] {
      assert_eq!(
        err::<HashMap<String, u8>>(scope, code),
        "prefix: context can not be converted to record",
        "{code}"
      );
    }
  }

  #[derive(Debug, PartialEq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct Dict {
    a: u8,
    #[options(clamp = true)]
    b: Vec<u16>,
    #[webidl(default = Some(3))]
    c: Option<u32>,
    #[webidl(rename = "e")]
    d: u16,
    f: HashMap<String, u32>,
    #[webidl(default = String::from("default"))]
    g: String,
    #[webidl(required)]
    h: Option<bool>,
    some_long_name: Option<String>,
    #[options(enforce_range = true)]
    #[webidl(default = 1)]
    z: u8,
  }

  #[test]
  fn dictionary() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(
      ok::<Dict>(
        scope,
        "({ a: 1, b: [1000000], e: 2, f: { x: 1 }, h: null })"
      ),
      Dict {
        a: 1,
        b: vec![65535],
        c: Some(3),
        d: 2,
        f: HashMap::from([("x".to_string(), 1)]),
        g: "default".to_string(),
        h: None,
        some_long_name: None,
        z: 1,
      }
    );
    assert_eq!(
      ok::<Dict>(
        scope,
        "({ a: 1, b: [], c: null, d: 5, e: 2, f: {}, g: undefined, h: true, someLongName: 'foo', some_long_name: 'bar', z: 7 })"
      ),
      Dict {
        a: 1,
        b: vec![],
        c: None,
        d: 2,
        f: HashMap::new(),
        g: "default".to_string(),
        h: Some(true),
        some_long_name: Some("foo".to_string()),
        z: 7,
      }
    );
    // Inherited members are read
    assert_eq!(
      ok::<Dict>(
        scope,
        "Object.create({ a: 1, b: [], e: 2, f: {}, h: false })"
      )
      .h,
      Some(false)
    );

    assert_eq!(
      err::<Dict>(scope, "({ b: [], e: 2, f: {}, h: true })"),
      "prefix: context can not be converted to 'Dict' because 'a' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "({ a: 1, b: [], e: 2, f: {} })"),
      "prefix: context can not be converted to 'Dict' because 'h' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "({ a: 1, b: [], e: 2, f: {}, h: undefined })"),
      "prefix: context can not be converted to 'Dict' because 'h' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "({ a: 1, b: [], d: 2, f: {}, h: true })"),
      "prefix: context can not be converted to 'Dict' because 'e' is required in 'Dict'"
    );
    assert_eq!(
      err::<Dict>(scope, "({ a: 1, b: 1, e: 2, f: {}, h: true })"),
      "prefix: 'b' of 'Dict' (context) can not be converted to sequence"
    );
    assert_eq!(
      err::<Dict>(scope, "({ a: 1, b: [], e: 2, f: {}, h: true, z: 256 })"),
      "prefix: 'z' of 'Dict' (context) is outside the accepted range of 0 to 255, inclusive"
    );
    assert_eq!(
      err::<Dict>(scope, "({ a: 1, b: [1, 2, Symbol()], e: 2, f: {}, h: true })"),
      "prefix: 'b' of 'Dict' (context), index 2 could not be converted: TypeError: Cannot convert a Symbol value to a number"
    );
    for code in ["undefined", "null"] {
      assert_eq!(
        err::<Dict>(scope, code),
        "prefix: context can not be converted to 'Dict' because 'a' is required in 'Dict'"
      );
    }
    for code in ["1", "'foo'", "true", "Symbol()"] {
      assert_eq!(
        err::<Dict>(scope, code),
        "prefix: context can not be converted to dictionary"
      );
    }
  }

  #[derive(Debug, PartialEq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct Ordered {
    zeta: u8,
    alpha: u8,
    #[webidl(rename = "beta")]
    omega: u8,
  }

  #[test]
  fn dictionary_member_order() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    let value = eval(
      scope,
      "globalThis.order = [];
      ({
        get zeta() { order.push('zeta'); return 3 },
        get beta() { order.push('beta'); return 2 },
        get alpha() { order.push('alpha'); return 1 },
      })",
    );
    let dict =
      Ordered::convert(scope, value, "prefix".into(), || "context".into(), &())
        .unwrap();
    assert_eq!(
      dict,
      Ordered {
        zeta: 3,
        alpha: 1,
        omega: 2
      }
    );
    let order = eval(scope, "order.join()");
    assert_eq!(order.to_rust_string_lossy(scope), "alpha,beta,zeta");

    // Conversion stops at the first error
    assert_eq!(
      err::<Ordered>(
        scope,
        "order = []; ({
          get zeta() { order.push('zeta'); return 3 },
          get alpha() { order.push('alpha'); throw new Error('boom') },
        })"
      ),
      "prefix: context could not be converted: Error: boom"
    );
    let order = eval(scope, "order.join()");
    assert_eq!(order.to_rust_string_lossy(scope), "alpha");
  }

  #[derive(Debug, PartialEq, crate::WebIDL)]
  #[webidl(dictionary)]
  struct WithLocal<'s> {
    value: Option<v8::Local<'s, v8::Value>>,
  }

  #[test]
  fn dictionary_with_lifetime() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();
    let dict = ok::<WithLocal>(scope, "({ value: 'foo' })");
    assert_eq!(dict.value.unwrap().to_rust_string_lossy(scope), "foo");
    let dict = ok::<WithLocal>(scope, "({})");
    assert!(dict.value.is_none());
  }

  #[derive(Debug, PartialEq, crate::WebIDL)]
  #[webidl(enum)]
  enum Enumeration {
    FooBar,
    Baz,
    #[webidl(rename = "hello")]
    World,
  }

  #[test]
  fn enumeration() {
    let mut runtime = JsRuntime::new(Default::default());
    let scope = &mut runtime.handle_scope();

    assert_eq!(ok::<Enumeration>(scope, "'foo-bar'"), Enumeration::FooBar);
    assert_eq!(ok::<Enumeration>(scope, "'baz'"), Enumeration::Baz);
    assert_eq!(ok::<Enumeration>(scope, "'hello'"), Enumeration::World);
    assert_eq!(
      ok::<Enumeration>(scope, "({ toString() { return 'baz' } })"),
      Enumeration::Baz
    );
    assert_eq!(
      ok::<Vec<Enumeration>>(scope, "['baz', 'hello']"),
      vec![Enumeration::Baz, Enumeration::World]
    );
    for (code, value) in [
      ("'world'", "world"),
      ("'FooBar'", "FooBar"),
      ("'fooBar'", "fooBar"),
      ("''", ""),
      ("undefined", "undefined"),
      ("1", "1"),
    ] {
      assert_eq!(
        err::<Enumeration>(scope, code),
        format!("prefix: context with value '{value}' is not a valid enum value of type 'Enumeration'")
      );
    }
    assert_eq!(
      err::<Enumeration>(scope, "Symbol()"),
      "prefix: context could not be converted: TypeError: Cannot convert a Symbol value to a string"
    );
  }
}
