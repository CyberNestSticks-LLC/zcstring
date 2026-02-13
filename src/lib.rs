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
//! ## serde_json example
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

#![cfg_attr(docsrs, feature(doc_cfg))]

use arcstr::{literal, ArcStr, Substr};
#[cfg(feature = "serde_json")]
use serde::{Deserialize, Deserializer, Serialize};
use std::cell::RefCell;
#[cfg(feature = "std")]
use std::io::{Read, Seek, SeekFrom};
use std::ops::Deref;
#[cfg(feature = "std")]
use std::ops::{Bound, RangeBounds};

thread_local! {
    /// The thread-local storage holding the current active source string.
    static SOURCE: RefCell<Option<ZCString>> =
        const { RefCell::new(None) };
}

// error for File, Read and Seek operations
#[cfg(feature = "std")]
#[derive(thiserror::Error, Debug)]
pub enum ReaderError {
    #[error("Invalid range: start {start} is greater than end {end}")]
    InvalidRange { start: u64, end: u64 },

    #[error("IO failure: {0}")]
    Io(#[from] std::io::Error),

    #[error("UTF-8 encoding failure: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// ZCString wrapper struct
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde_json", derive(Serialize))]
pub struct ZCString(Substr);

impl ZCString {
    /// Creates a new, empty `ZCString`.
    pub fn new() -> Self {
        ZCString::from(literal!(""))
    }

    /// Create an independent allocated copy of the underlying string
    /// buffer detached from the original string buffer.
    ///
    /// ```
    /// # use zcstring::ZCString;
    /// let large_source = ZCString::from_str_without_source("..."); // 1GB string
    /// let small_slice = large_source.substr(0..2);
    ///
    /// // Detach from the 1GB buffer to allow it to be garbage collected
    /// let owned_slice = small_slice.detach();
    /// ```
    pub fn detach(&self) -> Self {
        // create a new allocation
        ZCString::from_str_without_source(self.as_str())
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

    /// Creates a `ZCString` that uses a substr of the
    /// current `ZCString` if possible, otherwise allocate
    pub fn from_substr(&self, s: &str) -> Self {
        match (s.as_ptr() as usize).checked_sub(self.0.as_ptr() as usize) {
            Some(offset) if offset < self.0.len() => self.substr(offset..offset + s.len()),
            _ => ZCString::from_str_without_source(s),
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
        SOURCE.with(|ctx| match ctx.borrow().as_ref() {
            Some(source) => source.from_substr(s),
            None => ZCString::from_str_without_source(s),
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
    ///
    /// ### Example
    /// ```
    /// # use zcstring::ZCString;
    /// let source = ZCString::from("1 23 456 789 0");
    ///
    /// // Call a lambda function with our thread local storage
    /// // set to zc
    /// let result = ZCString::with_source(source, |source| {
    ///     // make it clear we are working with an &str
    ///     // borrowed from source
    ///     let s: &str = &source;
    ///     s
    ///         .split(' ')
    ///         // ZCString::from(v: &str) checks does &str lives in source?
    ///         .map(|v| ZCString::from(v))
    ///         // do we really point back to source?
    ///         .for_each(|v| assert!(source.source_of(&v)));
    /// });
    /// ```
    pub fn with_source<F, R>(source: ZCString, f: F) -> R
    where
        F: FnOnce(ZCString) -> R,
    {
        let guard = source.get_source_guard();
        let result = f(source);
        drop(guard);
        result
    }

    /// Transforms the current [`ZCString`] into a new view using a closure,
    /// provided the result is a sub-slice of the original.
    ///
    /// This is a high-level utility for performing zero-copy operations like
    /// trimming or pattern-based slicing using standard [`str`] methods.
    ///
    ///
    /// ### Example
    /// ```
    /// # use zcstring::ZCString;
    /// let zc = ZCString::from("  zero-copy  ");
    ///
    /// // Use map to trim the string without new allocations
    /// let trimmed = zc.map(|s| s.trim());
    ///
    /// assert_eq!(trimmed, "zero-copy");
    /// ```
    pub fn map<F>(&self, f: F) -> ZCString
    where
        F: FnOnce(&str) -> &str,
    {
        self.from_substr(f(self))
    }

    /// Wraps a standard string iterator to produce [`ZCString`] items instead of `&str`.
    ///
    /// This method allows you to leverage existing [`str`] iteration logic (like `.lines()` or `.split()`)
    /// while automatically promoting each yielded slice into a zero-copy [`ZCString`].
    ///
    /// The resulting items share the same underlying [`arcstr::ArcStr`] as this source,
    /// ensuring memory stays alive as long as any yielded item exists.
    ///
    /// ### Arguments
    /// * `f` - A closure that takes a reference to the inner string and returns an iterator yielding `&str`.
    ///
    /// ### Example
    /// ```
    /// # use zcstring::ZCString;
    /// let zc = ZCString::from("line1\nline2\nline3");
    ///
    /// // Wrap the standard .lines() iterator
    /// let mut iter = zc.wrap_iter(|s| s.lines());
    ///
    /// assert_eq!(iter.next().unwrap(), "line1");
    /// assert_eq!(iter.next().unwrap(), "line2");
    /// ```
    pub fn wrap_iter<'a, F, I>(&'a self, f: F) -> ZCStringIterWrapper<'a, I>
    where
        F: FnOnce(&'a str) -> I,
        I: Iterator<Item = &'a str>,
    {
        ZCStringIterWrapper {
            source: self.clone(),
            inner: f(self.as_str()),
            _marker: std::marker::PhantomData,
        }
    }

    #[cfg(feature = "std")]
    /// Create a ZCString by reading a range of bytes from a
    /// an object supporting Read and Seek traits. The range must
    /// contain valid UTF-8
    ///
    /// ### Arguments
    /// ```
    /// # use std::io::Cursor;
    /// # use zcstring::ZCString;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // test data in a form that supports Read & Seek traits
    /// // as if coming from a File
    /// let mut data = Cursor::new(b"Cats and dogs");
    /// // read "and" from 'data'
    /// let mut r = ZCString::read_range(&mut data, 5..8)?;
    /// assert_eq!(r, "and");
    /// # Ok(())
    /// # }
    /// ```
    pub fn read_range<I, R>(input: &mut I, range: R) -> Result<ZCString, ReaderError>
    where
        I: Read + Seek,
        R: RangeBounds<u64>,
    {
        let start_pos = match range.start_bound() {
            Bound::Included(s) => *s,
            Bound::Excluded(s) => *s + 1,
            Bound::Unbounded => input.stream_position()?,
        };

        let end_pos = match range.end_bound() {
            Bound::Included(e) => *e + 1,
            Bound::Excluded(e) => *e,
            Bound::Unbounded => input.seek(SeekFrom::End(0))?,
        };

        if start_pos > end_pos {
            // error
            return Err(ReaderError::InvalidRange {
                start: start_pos,
                end: end_pos,
            });
        }

        if start_pos == end_pos {
            // edge case
            return Ok(ZCString::new());
        }

        let mut io_error = Ok(());

        let result = ArcStr::init_with((end_pos - start_pos) as usize, |buffer| {
            io_error = (|| -> Result<(), std::io::Error> {
                input.seek(SeekFrom::Start(start_pos))?;
                input.read_exact(buffer)?;
                Ok(())
            })()
        })?;

        match io_error {
            Ok(()) => Ok(ZCString::from(result)),
            Err(e) => Err(e)?,
        }
    }

    #[cfg(feature = "std")]
    /// Create a ZCString by reading bytes from an object supporting the Read trait.
    /// The bytes must be valid UTF-8
    ///
    /// ### Arguments
    /// ```
    /// # use std::io::Cursor;
    /// # use zcstring::ZCString;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // test data in a form that supports Read & Seek traits
    /// // as if coming from a File
    /// let mut data = Cursor::new(b"Cats and dogs");
    /// // read "and" from 'data'
    /// let mut r = ZCString::read(&mut data, 4)?;
    /// assert_eq!(r, "Cats");
    /// # Ok(())
    /// # }
    /// ```
    pub fn read<I: Read>(input: &mut I, bytes: usize) -> Result<ZCString, ReaderError> {
        let mut io_error = Ok(());

        let result = ArcStr::init_with(bytes, |buffer| {
            io_error = (|| -> Result<(), std::io::Error> {
                input.read_exact(buffer)?;
                Ok(())
            })()
        })?;

        match io_error {
            Ok(()) => Ok(ZCString::from(result)),
            Err(e) => Err(e)?,
        }
    }

    #[cfg(feature = "std")]
    /// Create a ZCString by reading an entire file
    ///
    /// ### Arguments
    /// ```
    /// # use zcstring::ZCString;
    /// # use std::path::PathBuf;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// // Construct path relative to the project root
    /// let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    /// path.push("examples");
    /// path.push("from_file_test.txt");
    /// let r = ZCString::from_file(path)?;
    /// assert_eq!(&r, "xyzzy");
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_file<P: AsRef<std::path::Path>>(path: P) -> Result<ZCString, ReaderError> {
        let mut handle = std::fs::File::open(path)?;
        Self::read_range(&mut handle, 0..)
    }
}

impl Default for ZCString {
    fn default() -> Self {
        ZCString::from(literal!(""))
    }
}

impl PartialEq<str> for ZCString {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ZCString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<ZCString> for &str {
    fn eq(&self, other: &ZCString) -> bool {
        self == &**other
    }
}

#[cfg(feature = "std")]
impl PartialEq<String> for ZCString {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

#[cfg(feature = "std")]
impl PartialEq<ZCString> for String {
    fn eq(&self, other: &ZCString) -> bool {
        *self == other.0
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
        self
    }
}

impl std::borrow::Borrow<str> for ZCString {
    fn borrow(&self) -> &str {
        self
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

#[cfg(feature = "serde_json")]
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
#[cfg(feature = "serde_json")]
pub fn serde_json_from_zcstring<T>(json: ZCString) -> Result<T, serde_json::Error>
where
    T: for<'de> Deserialize<'de>,
{
    ZCString::with_source(json, |j| serde_json::from_str::<T>(&j))
}

/// str iterator wrapper automatically converts &str to ZCString
/// maintaining source references.
///
/// Use to wrap str iterators like lines()
pub struct ZCStringIterWrapper<'a, I> {
    source: ZCString,
    inner: I,
    _marker: std::marker::PhantomData<&'a str>,
}

impl<'a, I> Iterator for ZCStringIterWrapper<'a, I>
where
    I: Iterator<Item = &'a str>,
{
    type Item = ZCString;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|slice| self.source.from_substr(slice))
    }
}
