// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! WebIDL converters for compound types: nullable `T?`, `sequence<T>` and
//! `record<DOMString, T>`.

use std::borrow::Cow;
use std::collections::HashMap;

use super::fail;
use super::js;
use super::WebIdlConverter;
use super::WebIdlError;
use super::WebIdlErrorKind;

/// Nullable types: `undefined` and `null` become `None`, anything else is
/// converted with `T` and the same options.
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
      return Ok(None);
    }
    T::convert(scope, value, prefix, context, options).map(Some)
  }
}

/// `sequence<T>`: iterates the value with the iterator protocol. Each element
/// is converted with `T` and a context naming its index.
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
    let not_sequence =
      || fail(&prefix, &context, WebIdlErrorKind::NotASequence);
    let thrown =
      |e: String| fail(&prefix, &context, WebIdlErrorKind::Thrown(e));

    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(not_sequence());
    };

    let iterator_key: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let method = js(scope, |s| object.get(s, iterator_key)).map_err(thrown)?;
    let Ok(method) = v8::Local::<v8::Function>::try_from(method) else {
      return Err(not_sequence());
    };

    let this: v8::Local<v8::Value> = object.into();
    let iterator = js(scope, |s| method.call(s, this, &[])).map_err(thrown)?;
    let Ok(iterator) = v8::Local::<v8::Object>::try_from(iterator) else {
      return Err(not_sequence());
    };

    let next_key: v8::Local<v8::Value> =
      v8::String::new(scope, "next").unwrap().into();
    let next = js(scope, |s| iterator.get(s, next_key)).map_err(thrown)?;
    let Ok(next) = v8::Local::<v8::Function>::try_from(next) else {
      return Err(not_sequence());
    };

    let done_key: v8::Local<v8::Value> =
      v8::String::new(scope, "done").unwrap().into();
    let value_key: v8::Local<v8::Value> =
      v8::String::new(scope, "value").unwrap().into();
    let iterator_this: v8::Local<v8::Value> = iterator.into();

    let mut out = Vec::new();
    loop {
      let result =
        js(scope, |s| next.call(s, iterator_this, &[])).map_err(thrown)?;
      let Ok(result) = v8::Local::<v8::Object>::try_from(result) else {
        let kind = WebIdlErrorKind::Other(Cow::Borrowed(
          "returned a non-object iterator result",
        ));
        return Err(fail(&prefix, &context, kind));
      };
      let done = js(scope, |s| result.get(s, done_key)).map_err(thrown)?;
      if done.boolean_value(scope) {
        break;
      }
      let item = js(scope, |s| result.get(s, value_key)).map_err(thrown)?;
      let index = out.len();
      let element = T::convert(
        scope,
        item,
        prefix.clone(),
        || Cow::Owned(format!("{}, index {index}", context())),
        options,
      )?;
      out.push(element);
    }
    Ok(out)
  }
}

/// `record<DOMString, T>`: the own string keys are collected first, then each
/// key is checked with `[[GetOwnProperty]]` when it is reached, so a property
/// deleted or hidden by an earlier getter is skipped. Enumerable properties
/// are read with `Get` (so getters and proxy traps run) and converted with `T`.
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
    let Ok(object) = v8::Local::<v8::Object>::try_from(value) else {
      return Err(fail(&prefix, &context, WebIdlErrorKind::NotAnObject));
    };

    let args = v8::GetPropertyNamesArgs {
      mode: v8::KeyCollectionMode::OwnOnly,
      property_filter: v8::PropertyFilter::SKIP_SYMBOLS,
      index_filter: v8::IndexFilter::IncludeIndices,
      key_conversion: v8::KeyConversionMode::ConvertToString,
    };
    let names = js(scope, |s| object.get_own_property_names(s, args))
      .map_err(|e| fail(&prefix, &context, WebIdlErrorKind::Thrown(e)))?;

    let enumerable_key: v8::Local<v8::Value> =
      v8::String::new(scope, "enumerable").unwrap().into();
    let mut out = HashMap::new();
    for i in 0..names.length() {
      let key_value = js(scope, |s| names.get_index(s, i))
        .map_err(|e| fail(&prefix, &context, WebIdlErrorKind::Thrown(e)))?;
      // Keys from `ConvertToString` are always strings.
      let Ok(key_string) = v8::Local::<v8::String>::try_from(key_value) else {
        continue;
      };
      let name: v8::Local<v8::Name> = key_string.into();
      let descriptor =
        js(scope, |s| object.get_own_property_descriptor(s, name))
          .map_err(|e| fail(&prefix, &context, WebIdlErrorKind::Thrown(e)))?;
      // An undefined descriptor means the property was deleted by now.
      let Ok(descriptor) = v8::Local::<v8::Object>::try_from(descriptor) else {
        continue;
      };
      let enumerable = js(scope, |s| descriptor.get(s, enumerable_key))
        .map_err(|e| fail(&prefix, &context, WebIdlErrorKind::Thrown(e)))?;
      if !enumerable.boolean_value(scope) {
        continue;
      }
      let key = key_value.to_rust_string_lossy(scope);
      let item = js(scope, |s| object.get(s, key_value))
        .map_err(|e| fail(&prefix, &context, WebIdlErrorKind::Thrown(e)))?;
      let converted = T::convert(
        scope,
        item,
        prefix.clone(),
        || Cow::Owned(format!("'{key}' of record ({})", context())),
        options,
      )?;
      out.insert(key, converted);
    }
    Ok(out)
  }
}
