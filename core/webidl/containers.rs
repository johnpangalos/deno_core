// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

//! Converters for nullable types (`Option`), sequences (`Vec`) and records
//! (`HashMap<String, T>`).

use std::borrow::Cow;
use std::collections::HashMap;

use super::get_property;
use super::invalid;
use super::js;
use super::strings::to_dom_string;
use super::WebIdlConverter;
use super::WebIdlError;
use crate::v8;

/// `undefined` and `null` become `None`. Anything else is converted with `T`,
/// and `Options` is forwarded to `T`.
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Option<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &T::Options,
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

/// A WebIDL `sequence<T>`, read with the iterator protocol. Each element gets
/// the context `"{context}, index {i}"`.
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for Vec<T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &T::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Ok(object) = value.try_cast::<v8::Object>() else {
      return Err(invalid(&prefix, &context, "is not an object."));
    };
    let iterator_key: v8::Local<v8::Value> =
      v8::Symbol::get_iterator(scope).into();
    let method =
      js(scope, &prefix, &context, |tc| object.get(tc, iterator_key))?;
    let Ok(method) = method.try_cast::<v8::Function>() else {
      return Err(invalid(&prefix, &context, "is not iterable."));
    };
    let iterator = js(scope, &prefix, &context, |tc| {
      method.call(tc, object.into(), &[])
    })?;
    let Ok(iterator) = iterator.try_cast::<v8::Object>() else {
      return Err(invalid(&prefix, &context, "is not iterable."));
    };
    let next = get_property(scope, iterator, "next", &prefix, &context)?;
    let Ok(next) = next.try_cast::<v8::Function>() else {
      return Err(invalid(&prefix, &context, "is not iterable."));
    };

    let mut values = Vec::new();
    loop {
      let result = js(scope, &prefix, &context, |tc| {
        next.call(tc, iterator.into(), &[])
      })?;
      let Ok(result) = result.try_cast::<v8::Object>() else {
        return Err(invalid(
          &prefix,
          &context,
          "has an iterator whose next() did not return an object.",
        ));
      };
      let done = get_property(scope, result, "done", &prefix, &context)?;
      if done.boolean_value(scope) {
        break;
      }
      let element = get_property(scope, result, "value", &prefix, &context)?;
      let index = values.len();
      let element_context =
        || Cow::Owned(format!("{}, index {}", context(), index));
      values.push(T::convert(
        scope,
        element,
        prefix.clone(),
        element_context,
        options,
      )?);
    }
    Ok(values)
  }
}

/// A WebIDL `record<DOMString, T>`. Only own, enumerable string keys are read,
/// with `Get`. Each value gets the context `"{context}, key '{key}'"`.
impl<'a, T: WebIdlConverter<'a>> WebIdlConverter<'a> for HashMap<String, T> {
  type Options = T::Options;

  fn convert<C>(
    scope: &mut v8::HandleScope<'a>,
    value: v8::Local<'a, v8::Value>,
    prefix: Cow<'static, str>,
    context: C,
    options: &T::Options,
  ) -> Result<Self, WebIdlError>
  where
    C: Fn() -> Cow<'static, str>,
  {
    let Ok(object) = value.try_cast::<v8::Object>() else {
      return Err(invalid(&prefix, &context, "is not an object."));
    };
    let args = v8::GetPropertyNamesArgs {
      mode: v8::KeyCollectionMode::OwnOnly,
      property_filter: v8::PropertyFilter::ALL_PROPERTIES,
      index_filter: v8::IndexFilter::IncludeIndices,
      key_conversion: v8::KeyConversionMode::ConvertToString,
    };
    let keys = js(scope, &prefix, &context, |tc| {
      object.get_own_property_names(tc, args)
    })?;

    let mut map = HashMap::new();
    for index in 0..keys.length() {
      let key = js(scope, &prefix, &context, |tc| keys.get_index(tc, index))?;
      if !is_enumerable_own(scope, object, key, &prefix, &context)? {
        continue;
      }
      let name = to_dom_string(scope, key, &prefix, &context)?;
      let value = {
        let key_context =
          || Cow::Owned(format!("{}, key '{}'", context(), name));
        let element =
          js(scope, &prefix, &key_context, |tc| object.get(tc, key))?;
        T::convert(scope, element, prefix.clone(), key_context, options)?
      };
      map.insert(name, value);
    }
    Ok(map)
  }
}

/// Whether `key` is an own enumerable property. Missing properties (for
/// example a Proxy that lists keys it does not have) are not enumerable.
fn is_enumerable_own<'a>(
  scope: &mut v8::HandleScope<'a>,
  object: v8::Local<'a, v8::Object>,
  key: v8::Local<'a, v8::Value>,
  prefix: &str,
  context: &dyn Fn() -> Cow<'static, str>,
) -> Result<bool, WebIdlError> {
  let Ok(name) = key.try_cast::<v8::Name>() else {
    return Ok(false);
  };
  let descriptor = js(scope, prefix, context, |tc| {
    object.get_own_property_descriptor(tc, name)
  })?;
  let Ok(descriptor) = descriptor.try_cast::<v8::Object>() else {
    return Ok(false);
  };
  let enumerable =
    get_property(scope, descriptor, "enumerable", prefix, context)?;
  Ok(enumerable.boolean_value(scope))
}

#[cfg(test)]
mod tests {
  use std::borrow::Cow;

  use super::*;
  use crate::webidl::with_js_value;
  use crate::webidl::IntOptions;

  fn prefix() -> Cow<'static, str> {
    Cow::Borrowed("P")
  }

  fn context() -> Cow<'static, str> {
    Cow::Borrowed("C")
  }

  fn option_u8(
    source: &str,
    options: IntOptions,
  ) -> Result<Option<u8>, String> {
    with_js_value(source, |scope, value| {
      Option::<u8>::convert(scope, value, prefix(), context, &options)
        .map_err(|e| e.to_string())
    })
  }

  fn seq_u8(source: &str) -> Result<Vec<u8>, String> {
    with_js_value(source, |scope, value| {
      Vec::<u8>::convert(
        scope,
        value,
        prefix(),
        context,
        &IntOptions::default(),
      )
      .map_err(|e| e.to_string())
    })
  }

  fn seq_u8_with(source: &str, options: IntOptions) -> Result<Vec<u8>, String> {
    with_js_value(source, |scope, value| {
      Vec::<u8>::convert(scope, value, prefix(), context, &options)
        .map_err(|e| e.to_string())
    })
  }

  fn record_string(source: &str) -> Result<HashMap<String, String>, String> {
    with_js_value(source, |scope, value| {
      HashMap::<String, String>::convert(
        scope,
        value,
        prefix(),
        context,
        &Default::default(),
      )
      .map_err(|e| e.to_string())
    })
  }

  fn record_u8(source: &str) -> Result<HashMap<String, u8>, String> {
    with_js_value(source, |scope, value| {
      HashMap::<String, u8>::convert(
        scope,
        value,
        prefix(),
        context,
        &IntOptions::default(),
      )
      .map_err(|e| e.to_string())
    })
  }

  #[test]
  fn option_null_and_undefined_are_none() {
    let default = IntOptions::default();
    assert_eq!(option_u8("undefined", default), Ok(None));
    assert_eq!(option_u8("null", default), Ok(None));
    assert_eq!(option_u8("0", default), Ok(Some(0)));
  }

  #[test]
  fn option_forwards_options() {
    let clamp = IntOptions {
      enforce_range: false,
      clamp: true,
    };
    assert_eq!(option_u8("300", clamp), Ok(Some(255)));
  }

  #[test]
  fn option_js_exception_is_an_error() {
    assert_eq!(
      option_u8("({valueOf(){throw 1}})", IntOptions::default()),
      Err("P: C threw an exception: 1".into())
    );
  }

  #[test]
  fn sequence_rejects_non_objects() {
    assert_eq!(seq_u8("'abc'"), Err("P: C is not an object.".into()));
    assert_eq!(seq_u8("5"), Err("P: C is not an object.".into()));
    assert_eq!(seq_u8("null"), Err("P: C is not an object.".into()));
    assert_eq!(seq_u8("undefined"), Err("P: C is not an object.".into()));
  }

  #[test]
  fn sequence_rejects_non_iterables() {
    assert_eq!(seq_u8("({})"), Err("P: C is not iterable.".into()));
    assert_eq!(seq_u8("({length: 2})"), Err("P: C is not iterable.".into()));
    assert_eq!(
      seq_u8("({[Symbol.iterator]: 1})"),
      Err("P: C is not iterable.".into())
    );
  }

  #[test]
  fn sequence_reads_iterables() {
    assert_eq!(seq_u8("[]"), Ok(vec![]));
    assert_eq!(seq_u8("[1, 2, 3]"), Ok(vec![1, 2, 3]));
    assert_eq!(seq_u8("[1,,3]"), Ok(vec![1, 0, 3]));
    assert_eq!(seq_u8("new Set([1, 2])"), Ok(vec![1, 2]));
    assert_eq!(
      seq_u8("(function*(){ yield 1; yield 2; })()"),
      Ok(vec![1, 2])
    );
    assert_eq!(
      seq_u8("(function(){ return arguments })(4, 5)"),
      Ok(vec![4, 5])
    );
    assert_eq!(
      seq_u8(
        "({[Symbol.iterator]() { let i = 0; return { next() { i++; return { done: i > 2, value: i } } } } })"
      ),
      Ok(vec![1, 2])
    );
  }

  #[test]
  fn sequence_element_error_names_index() {
    assert_eq!(
      seq_u8("[1, Symbol()]"),
      Err(
        "P: C, index 1 is a Symbol and cannot be converted to a number.".into()
      )
    );
  }

  #[test]
  fn sequence_element_options_are_forwarded() {
    let clamp = IntOptions {
      enforce_range: false,
      clamp: true,
    };
    assert_eq!(seq_u8_with("[300, -1]", clamp), Ok(vec![255, 0]));
  }

  #[test]
  fn sequence_js_exceptions_are_errors() {
    assert_eq!(
      seq_u8("(function*(){ yield 1; throw new Error('mid'); })()"),
      Err("P: C threw an exception: Error: mid".into())
    );
    assert_eq!(
      seq_u8("({ get [Symbol.iterator]() { throw new Error('getter') } })"),
      Err("P: C threw an exception: Error: getter".into())
    );
    assert_eq!(
      seq_u8("({[Symbol.iterator]() { return { next() { throw 7 } } }})"),
      Err("P: C threw an exception: 7".into())
    );
  }

  #[test]
  fn sequence_next_returning_non_object_is_an_error() {
    assert_eq!(
      seq_u8("({[Symbol.iterator]() { return { next() { return 1 } } }})"),
      Err("P: C has an iterator whose next() did not return an object.".into())
    );
  }

  #[test]
  fn record_rejects_non_objects() {
    assert_eq!(record_string("null"), Err("P: C is not an object.".into()));
    assert_eq!(
      record_string("undefined"),
      Err("P: C is not an object.".into())
    );
    assert_eq!(record_string("'str'"), Err("P: C is not an object.".into()));
  }

  #[test]
  fn record_reads_own_enumerable_string_keys() {
    assert_eq!(record_string("({})"), Ok(HashMap::new()));
    assert_eq!(
      record_string("({a: 'x', 1: 'y'})"),
      Ok(HashMap::from([
        ("a".to_string(), "x".to_string()),
        ("1".to_string(), "y".to_string()),
      ]))
    );
    assert_eq!(
      record_u8("({a: 1, 'b c': 2})"),
      Ok(HashMap::from([
        ("a".to_string(), 1),
        ("b c".to_string(), 2)
      ]))
    );
  }

  #[test]
  fn record_skips_non_enumerable_missing_and_inherited() {
    assert_eq!(
      record_string(
        "(function(){ const o = {a: 'x'}; Object.defineProperty(o, 'hidden', {value: 'y', enumerable: false}); return o })()"
      ),
      Ok(HashMap::from([("a".to_string(), "x".to_string())]))
    );
    assert_eq!(
      record_string("({__proto__: {inherited: 'z'}})"),
      Ok(HashMap::new())
    );
    assert_eq!(
      record_string("new Proxy({}, { ownKeys() { return ['ghost'] } })"),
      Ok(HashMap::new())
    );
  }

  #[test]
  fn record_enumerable_symbol_key_is_an_error() {
    assert_eq!(
      record_string("({[Symbol()]: 'x'})"),
      Err("P: C is a Symbol and cannot be converted to a string.".into())
    );
    assert_eq!(
      record_string(
        "(function(){ const o = {a: 'x'}; Object.defineProperty(o, Symbol(), {value: 'y', enumerable: false}); return o })()"
      ),
      Ok(HashMap::from([("a".to_string(), "x".to_string())]))
    );
  }

  #[test]
  fn record_value_error_names_key() {
    assert_eq!(
      record_u8("({a: Symbol()})"),
      Err(
        "P: C, key 'a' is a Symbol and cannot be converted to a number.".into()
      )
    );
  }

  #[test]
  fn record_js_exceptions_are_errors() {
    assert_eq!(
      record_string("({ get a() { throw new Error('g') } })"),
      Err("P: C, key 'a' threw an exception: Error: g".into())
    );
    assert_eq!(
      record_string("new Proxy({}, { ownKeys() { throw new Error('trap') } })"),
      Err("P: C threw an exception: Error: trap".into())
    );
  }
}
