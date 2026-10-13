// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use proc_macro2::Ident;
use proc_macro2::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::DataStruct;
use syn::Error;
use syn::Expr;
use syn::Fields;
use syn::Lifetime;
use syn::LitStr;
use syn::Type;

struct Member {
  /// The variable holding the converted member.
  var: Ident,
  ident: Ident,
  ty: Type,
  key: String,
  default: Option<Expr>,
  required: bool,
  options: Vec<(Ident, Expr)>,
}

impl Member {
  fn parse(index: usize, field: &syn::Field) -> Result<Self, Error> {
    let ident = field.ident.clone().unwrap();
    let mut key = stringcase::camel_case(&ident.unraw().to_string());
    let mut default = None;
    let mut required = false;
    let mut options = vec![];

    for attr in &field.attrs {
      if attr.path().is_ident("webidl") {
        attr.parse_nested_meta(|meta| {
          if meta.path.is_ident("rename") {
            key = meta.value()?.parse::<LitStr>()?.value();
          } else if meta.path.is_ident("default") {
            default = Some(meta.value()?.parse::<Expr>()?);
          } else if meta.path.is_ident("required") {
            required = true;
          } else {
            return Err(meta.error(
              "expected `rename = \"...\"`, `default = ...` or `required`",
            ));
          }
          Ok(())
        })?;
      } else if attr.path().is_ident("options") {
        attr.parse_nested_meta(|meta| {
          let Some(option) = meta.path.get_ident().cloned() else {
            return Err(meta.error("expected an option name"));
          };
          options.push((option, meta.value()?.parse::<Expr>()?));
          Ok(())
        })?;
      }
    }

    if required && default.is_some() {
      return Err(Error::new(
        field.span(),
        "a member can not be both required and have a default",
      ));
    }

    Ok(Self {
      var: format_ident!("__member{index}"),
      ident,
      ty: field.ty.clone(),
      key,
      default,
      required,
      options,
    })
  }
}

fn is_option(ty: &Type) -> bool {
  let Type::Path(path) = ty else {
    return false;
  };
  path.qself.is_none()
    && path
      .path
      .segments
      .last()
      .is_some_and(|segment| segment.ident == "Option")
}

pub fn convert(
  name: &str,
  lifetime: &Lifetime,
  data: &DataStruct,
) -> Result<TokenStream, Error> {
  let Fields::Named(fields) = &data.fields else {
    return Err(Error::new(
      data.struct_token.span(),
      "dictionaries must have named fields",
    ));
  };

  let mut members = fields
    .named
    .iter()
    .enumerate()
    .map(|(index, field)| Member::parse(index, field))
    .collect::<Result<Vec<_>, _>>()?;

  // Members are converted in lexicographical order of their keys.
  members.sort_by(|a, b| a.key.cmp(&b.key));
  for pair in members.windows(2) {
    if pair[0].key == pair[1].key {
      return Err(Error::new(
        pair[1].ident.span(),
        format!("duplicate dictionary member '{}'", pair[1].key),
      ));
    }
  }

  let conversions = members.iter().map(|member| {
    let Member {
      var,
      ty,
      key,
      default,
      required,
      options,
      ..
    } = member;
    let member_context = format!("'{key}' of '{name}'");

    let options = if options.is_empty() {
      quote!(&::std::default::Default::default())
    } else {
      let options = options.iter().map(|(option, value)| {
        quote!(__options.#option = #value;)
      });
      quote!({
        let mut __options = <#ty as ::deno_core::webidl::WebIdlConverter<#lifetime>>::Options::default();
        #(#options)*
        &{ __options }
      })
    };

    let missing = if let Some(default) = default {
      quote!(#default)
    } else if *required || !is_option(ty) {
      quote!(
        return ::std::result::Result::Err(::deno_core::webidl::WebIdlError::new(
          __prefix,
          &__context,
          ::deno_core::webidl::WebIdlErrorKind::DictionaryCannotConvertKey {
            converter: #name,
            key: #key,
          },
        ))
      )
    } else {
      quote!(::std::option::Option::None)
    };

    quote! {
      let #var: #ty = match ::deno_core::webidl::dictionary_member(
        __scope,
        __object,
        #key,
        &__prefix,
        &__context,
      )? {
        ::std::option::Option::Some(__value) => {
          <#ty as ::deno_core::webidl::WebIdlConverter<#lifetime>>::convert(
            __scope,
            __value,
            __prefix.clone(),
            || ::std::format!("{} ({})", #member_context, __context()).into(),
            #options,
          )?
        }
        ::std::option::Option::None => #missing,
      };
    }
  });

  let fields = members
    .iter()
    .map(|Member { var, ident, .. }| quote!(#ident: #var));

  Ok(quote! {
    let __object = ::deno_core::webidl::dictionary_object(__value, &__prefix, &__context)?;
    #(#conversions)*
    ::std::result::Result::Ok(Self { #(#fields),* })
  })
}
