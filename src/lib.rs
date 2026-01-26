// Copyright (c) 2026 CyberNestSticks LLC
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Author: Lawrence (Larry) Foard

//! # ZCString
//!
//! `ZCString` is a wrapper around [`arcstr::Substr`] designed for zero-copy
//! parsing using a thread-local context. It allows you to track a "source"
//! string and derive substrings from it without unnecessary allocations.
//! serde_json is currently supported by default.
//!
//! ## Main Functionality
//! - **Context-aware creation**: Uses a thread-local `SOURCE` to check if a new string
//!   is actually a sub-slice of an existing managed string.
//! - **RAII Guards**: Provides a [`SourceGuard`] to safely manage the lifecycle of the
//!   thread-local source.
//! - **Serde Integration**: Optional (defaults to on) support for efficient 
//!   zero-copy deserialization via the `serde` feature flag.
//!
//! ## Crate Features
//!
//! * **`default`** By default, serde and std are enabled.
//! * **`serde`** (Optional): Enables serialization and deserialization support via `serde` and `serde_json`.
//! * **`std`** (Optional): Enables `From<String>` implementations.
//! ## Trivial Example
//!
//! ```rust
//! use arcstr::literal;
//! use serde::Deserialize;
//! use std::error::Error;
//! use zcstring::{ZCString, serde_json_from_zcstring};
//!
//! #[derive(Debug, Deserialize)]
//! struct Animal {
//!    animal: ZCString,
//!    color: ZCString,
//! }
//! 
//! fn main() -> Result<(), Box<dyn Error>> {
//!     let json = literal!(r#"{"animal":"cat", "color": "red"}"#);
//!     // ZCString::from below is zero-copy from the ArcStr json
//!     // when done struct members will point to memory within
//!     // the json string.
//!     let animal = serde_json_from_zcstring::<Animal>(json.into())?;
//!     println!("animal: {:?}", animal);
//!     Ok(())
//! }
//!```

use arcstr::{literal, ArcStr, Substr};
#[cfg(feature = "serde")]
use serde::{Deserialize, Deserializer, Serialize};
use std::cell::RefCell;
use std::error::Error;
use std::ops::Deref;
use std::ops::RangeBounds;

thread_local! {
    /// The thread-local storage holding the current active source string.
    static SOURCE: RefCell<Option<ZCString>> =
        const { RefCell::new(None) };
}

/// ZCString wrapper struct
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize))]
pub struct ZCString(Substr);

impl ZCString {
    /// Creates a new, empty `ZCString`.
    pub fn new() -> Self {
        ZCString::from(literal!(""))
    }

    /// Returns `true` if the string slice `s` physically resides within the 
    /// memory bounds of this `ZCString`.
    ///
    /// ### Example
    /// ```
    /// # use zcstring::ZCString;
    /// let root = ZCString::from_str_without_source("hello world");
    /// let sub = &root[0..5];
    /// assert!(root.source_of(sub));
    /// ```
    pub fn source_of(&self, s: &str) -> bool {
        if let Some(offset) = (s.as_ptr() as usize).checked_sub(self.0.as_ptr() as usize) {
            // do we fall within?
            offset < self.0.len()
        } else {
            // we fall below the source
            false
        }
    }

    /// Creates a `ZCString` by allocating a new `ArcStr`. 
    /// 
    /// This bypasses the thread-local source check and just allocates.
    pub fn from_str_without_source(s: &str) -> Self {
        ZCString(Substr::from(ArcStr::from(s)))
    }

    /// Creates a `ZCString` by checking if `s` is a sub-slice of the current 
    /// thread-local `SOURCE`. 
    ///
    /// If `s` is found within the source, it returns a pointer-based sub-slice.
    /// Otherwise, it falls back to [`Self::from_str_without_source`].
    pub fn from_str_with_source(s: &str) -> Self {
        SOURCE.with(|ctx| {
            ctx.borrow()
                .as_ref()
                .and_then(|source| {
                    let offset = (s.as_ptr() as usize).checked_sub(source.as_ptr() as usize)?;

                    if offset < source.len() {
                        Some(source.substr(offset..offset + s.len()))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| ZCString::from_str_without_source(s))
        })
    }

    /// Returns a sub-slice of this `ZCString` as a new `ZCString`.
    pub fn substr(&self, range: impl RangeBounds<usize>) -> Self {
        ZCString(self.0.substr(range))
    }

    /// Returns an RAII [`SourceGuard`] that sets this string as the thread-local 
    /// source. When the guard is dropped, the previous source is restored.
    pub fn get_source_guard(&self) -> SourceGuard {
        let mut source = Some(self.clone());

        SOURCE.with(|ctx| {
            let mut borrow = ctx.borrow_mut();
            std::mem::swap(&mut *borrow, &mut source);
        });

        SourceGuard { old_source: source }
    }

    /// Executes a closure with this `ZCString` set as the thread-local source.
    ///
    /// This is the preferred way to handle contextual string operations.
    pub fn with_source<F, R>(source: ZCString, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let guard = source.get_source_guard();
        let result = f();
        drop(guard);
        result
    }
}

impl Default for ZCString {
    fn default() -> Self {
        ZCString::from(literal!(""))
    }
}

impl PartialEq<str> for ZCString {
    fn eq(&self, other: &str) -> bool {
        &**self == other
    }
}

impl Deref for ZCString {
    type Target = Substr;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for ZCString {
    fn as_ref(&self) -> &str {
        &**self
    }
}

impl std::borrow::Borrow<str> for ZCString {
    fn borrow(&self) -> &str {
        &**self
    }
}

impl std::fmt::Display for ZCString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::fmt::Debug for ZCString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

/// From<&str> will check for existence of &str within the current source
//             ZCString
impl From<&str> for ZCString {
    #[inline]
    fn from(s: &str) -> Self {
        ZCString::from_str_with_source(s)
    }
}

impl From<ArcStr> for ZCString {
    #[inline]
    fn from(s: ArcStr) -> Self {
        ZCString(Substr::from(s))
    }
}

#[cfg(feature = "std")]
impl From<String> for ZCString {
    #[inline]
    fn from(s: String) -> Self {
        ZCString::from_str_without_source(&s)
    }
}

/// An RAII guard used to manage the lifecycle of the thread-local string source.
/// 
/// Created via [`ZCString::get_source_guard`].
pub struct SourceGuard {
    old_source: Option<ZCString>,
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        SOURCE.with(|ctx| {
            let mut borrow = ctx.borrow_mut();
            std::mem::swap(&mut *borrow, &mut self.old_source);
        });
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for ZCString {
    /// Custom deserializer that attempts to borrow from the thread-local source 
    /// when encountering a string.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ZCStringVisitor;

        impl<'de> serde::de::Visitor<'de> for ZCStringVisitor {
            type Value = ZCString;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string that can be borrowed or owned")
            }

            // borrow will build an arcstr::Substr of the original JSON
            fn visit_borrowed_str<E>(self, s: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ZCString::from_str_with_source(s))
            }

            // build an arcstr::Substr based on the full ArcStr of our
            // decoded string
            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ZCString::from_str_without_source(s))
            }

            // build an arcstr::Substr based on the full ArcStr of our
            // decoded string
            fn visit_string<E>(self, s: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(s.as_str())
            }
        }

        // deserialize using our visitor
        deserializer.deserialize_str(ZCStringVisitor)
    }
}

/// Parses a JSON string into type `T` while using the provided `ZCString` as 
/// the context for any zero-copy deserialization.
///
/// **Requires the `serde` feature.**
#[cfg(feature = "serde")]
pub fn serde_json_from_zcstring<T>(json: ZCString) -> Result<T, Box<dyn Error>>
where
    T: for<'de> Deserialize<'de>,
{
    Ok(ZCString::with_source(json.clone(), || {
        serde_json::from_str::<T>(&json)
    })?)
}
