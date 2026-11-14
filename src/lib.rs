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
//!   zero-copy deserialization via the `serde_json` feature flag.
//!
//! ## Crate Features
//!
//! * **`default`** By default, serde_json is enabled.
//! * **`serde_json`** (Optional): Enables serialization and deserialization support via `serde` and `serde_json`.
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

use arcstr::{ArcStr, Substr};

// re-export
pub use arcstr::{literal, literal_substr};
#[cfg(feature = "serde_json")]
use serde::{Deserialize, Deserializer, Serialize, Serializer};
#[cfg(feature = "serde_json")]
use serde::de::{DeserializeOwned};
#[cfg(feature = "serde_json")]
use serde::ser::Error;
#[cfg(feature = "serde_json")]
use serde_json::value::RawValue;
use core::cell::RefCell;
#[cfg(feature = "serde_json")]
use std::cmp::Ordering;
#[cfg(feature = "serde_json")]
use std::fmt::{self, Debug};
#[cfg(feature = "serde_json")]
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Deref;
use core::ops::{Bound, RangeBounds};
#[cfg(feature = "serde_json")]
use std::sync::{Arc, OnceLock};


thread_local! {
    /// The thread-local storage holding the current active source string.
    static SOURCE: RefCell<Option<ZCString>> =
        const { RefCell::new(None) };
}

/// error for File, Read and Seek operations
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
    pub const fn new() -> Self {
        ZCString(literal_substr!(""))
    }

    /// Shorthand to wrap literal_substr!("...")
    pub const fn l(v: Substr) -> Self {
        ZCString(v)
    }

    /// Create an independent allocated copy of the underlying string
    /// buffer detached from the original string buffer.
    ///
    /// ```
    /// # use zcstring::ZCString;
    /// # use arcstr::literal_substr;
    /// const EMPTY: ZCString = ZCString::new();
    /// const X: ZCString = ZCString::l(literal_substr!("x"));
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
        let self_start = self.0.as_ptr() as usize;
        let s_start = s.as_ptr() as usize;
        if let Some(offset) = s_start.checked_sub(self_start) {
            offset.checked_add(s.len()).is_some_and(|end| end <= self.0.len())
        } else {
            false
        }
    }

    /// Creates a `ZCString` that uses a substr of the
    /// current `ZCString` if possible, otherwise allocate
    pub fn from_substr(&self, s: &str) -> Self {
        let self_start = self.0.as_ptr() as usize;
        let s_start = s.as_ptr() as usize;
        match s_start.checked_sub(self_start) {
            Some(offset) if offset.checked_add(s.len()).is_some_and(|end| end <= self.0.len()) => {
                self.substr(offset..offset + s.len())
            }
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

    /// Create a ZCString by reading a range of bytes from
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
            Bound::Included(&s) => Some(s),
            Bound::Excluded(&s) => s.checked_add(1),
            Bound::Unbounded => Some(input.stream_position()?),
        };

        let end_pos = match range.end_bound() {
            Bound::Included(&e) => e.checked_add(1),
            Bound::Excluded(&e) => Some(e),
            Bound::Unbounded => Some(input.seek(SeekFrom::End(0))?),
        };

        let (Some(start_pos), Some(end_pos)) = (start_pos, end_pos) else {
            return Err(ReaderError::InvalidRange {
                start: start_pos.unwrap_or(u64::MAX),
                end: end_pos.unwrap_or(u64::MAX),
            });
        };

        if start_pos > end_pos {
            return Err(ReaderError::InvalidRange {
                start: start_pos,
                end: end_pos,
            });
        }

        let stream_len = match range.end_bound() {
            Bound::Unbounded => end_pos,
            _ => input.seek(SeekFrom::End(0))?,
        };

        if end_pos > stream_len {
            return Err(ReaderError::InvalidRange {
                start: start_pos,
                end: end_pos,
            });
        }

        let len = end_pos - start_pos;
        if len == 0 {
            return Ok(ZCString::new());
        }

        let len = usize::try_from(len).map_err(|_| ReaderError::InvalidRange {
            start: start_pos,
            end: end_pos,
        })?;

        input.seek(SeekFrom::Start(start_pos))?;

        let mut io_error = Ok(());

        let result = ArcStr::init_with(len, |buffer| {
            io_error = (|| -> Result<(), std::io::Error> {
                input.seek(SeekFrom::Start(start_pos))?;
                input.read_exact(buffer)?;
                Ok(())
            })()
        });

        // deal with one of two error sources, I/O errors take priority
        match (io_error, result) {
            (Ok(()), Ok(result)) => Ok(ZCString::from(result)),
            (Err(e), _) => Err(e)?,
            (_, Err(e)) => Err(e)?,
        }
    }

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

impl PartialEq<String> for ZCString {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

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

/// ZCRawValue serde type that contains an internal ZCString
/// allows convenient retrieval of raw JSON
///
/// ```
/// # use zcstring::{ZCRawValue, ZCString, serde_json_from_zcstring};
/// # use arcstr::literal_substr;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// const EMPTY: ZCString = ZCString::new();
/// const json: ZCString = ZCString::l(literal_substr!("{\"a\": \"b\", \"c\":[\"a\", \"b\", 3]}"));
/// #[derive(serde::Deserialize)]
/// struct X {
///     a: ZCString,
///     c: ZCRawValue,
/// }
/// let x: X = serde_json_from_zcstring(json)?;
/// assert_eq!(x.c.value(), "[\"a\", \"b\", 3]");
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "serde_json")]
#[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ZCRawValue(ZCString);

#[cfg(feature = "serde_json")]
impl ZCRawValue {
    pub fn new(v: ZCString) -> Self {
        ZCRawValue(v)
    }

    pub fn value(&self) -> &ZCString {
        &self.0
    }
}

#[cfg(feature = "serde_json")]
impl std::fmt::Debug for ZCRawValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0.as_str(), f)
    }
}

#[cfg(feature = "serde_json")]
impl<'de> Deserialize<'de> for ZCRawValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw_val: &'de RawValue =
            Deserialize::deserialize(deserializer)?;

        Ok(ZCRawValue(ZCString::from_str_with_source(raw_val.get())))
    }
}

#[cfg(feature = "serde_json")]
impl Serialize for ZCRawValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {    
        let raw = serde_json::from_str::<&serde_json::value::RawValue>(self.0.as_str())
            .map_err(S::Error::custom)?;
        raw.serialize(serializer)
    }
}

/// ZCRawValueAnd serde type that contains both a ZCRawValue and a generic
///               type
///
/// ```
/// # use zcstring::{ZCRawValueAnd, ZCString, serde_json_from_zcstring};
/// # use arcstr::literal_substr;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// const EMPTY: ZCString = ZCString::new();
/// const json: ZCString = ZCString::l(literal_substr!("{\"a\": \"b\", \"c\":[1, 2,  3]}"));
/// #[derive(serde::Deserialize)]
/// struct X {
///     a: ZCString,
///     c: ZCRawValueAnd<Vec<u32>>,
/// }
/// let x: X = serde_json_from_zcstring(json)?;
/// assert_eq!(x.c.raw(), "[1, 2,  3]");
/// assert_eq!(x.c.value().as_slice(), [1, 2, 3]);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "serde_json")]
pub struct ZCRawValueAnd<T> {
    raw: ZCRawValue,
    value: T,
}

#[cfg(feature = "serde_json")]
impl<T> ZCRawValueAnd<T> {
    pub fn raw(&self) -> &ZCString {
        self.raw.value()
    }

    pub fn value(&self) -> &T {
        &self.value
    }
}

#[cfg(feature = "serde_json")]
impl<T> Deref for ZCRawValueAnd<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[cfg(feature = "serde_json")]
impl<'de, T: DeserializeOwned> Deserialize<'de> for ZCRawValueAnd<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: ZCRawValue = 
            Deserialize::deserialize(deserializer)?;
            
        // now deserialize the data component from our raw string
        // raw is a substring of our data source, we don't need
        // to re-specify it.
        let value =
            serde_json::from_str(raw.value().as_str())
            .map_err(serde::de::Error::custom)?;

        Ok(ZCRawValueAnd {
            raw,
            value,
        })
    }
}

#[cfg(feature = "serde_json")]
impl<T: Serialize> Serialize for ZCRawValueAnd<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // just send back the raw version
        self.raw.serialize(serializer)
    }
}

#[cfg(feature = "serde_json")]
impl<T: fmt::Debug> fmt::Debug for ZCRawValueAnd<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.value, f)
    }
}

#[cfg(feature = "serde_json")]
impl<T: fmt::Display> fmt::Display for ZCRawValueAnd<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.value, f)
    }
}

#[cfg(feature = "serde_json")]
impl<T: Clone> Clone for ZCRawValueAnd<T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            value: self.value.clone(),
        }
    }
}

#[cfg(feature = "serde_json")]
impl<T: PartialEq> PartialEq for ZCRawValueAnd<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value.eq(&other.value)
    }
}

#[cfg(feature = "serde_json")]
impl<T: Eq> Eq for ZCRawValueAnd<T> {}

#[cfg(feature = "serde_json")]
impl<T: PartialOrd> PartialOrd for ZCRawValueAnd<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.value.partial_cmp(&other.value)
    }
}

#[cfg(feature = "serde_json")]
impl<T: Ord> Ord for ZCRawValueAnd<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

#[cfg(feature = "serde_json")]
impl<T: Hash> Hash for ZCRawValueAnd<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

#[cfg(feature = "serde_json")]
impl<T: Default> Default for ZCRawValueAnd<T> {
    fn default() -> Self {
        Self {
            raw: ZCRawValue::default(),
            value: T::default(),
        }
    }
}

/// ZCLazyValue serde type that contains both a ZCRawValue and a generic
///               type that's parsed only on access
///
/// ```
/// # use zcstring::{ZCLazyValue, ZCString, serde_json_from_zcstring};
/// # use arcstr::literal_substr;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// const EMPTY: ZCString = ZCString::new();
/// let json: ZCString = ZCString::l(literal_substr!("{\"a\": \"b\", \"c\":[1, 2,  3]}"));
/// #[derive(serde::Deserialize, serde::Serialize)]
/// struct X {
///     a: ZCString,
///    #[serde(default, skip_serializing_if = "ZCLazyValue::is_default")]
///     c: ZCLazyValue<Vec<u32>>,
/// }
///
/// let x: X = serde_json_from_zcstring(json)?;
/// assert_eq!(x.c.raw(), "[1, 2,  3]");
/// assert!(!x.c.decoded());
/// assert_eq!(x.c.value().unwrap().as_slice(), [1, 2, 3]);
/// assert!(x.c.decoded());
///
/// // bad JSON
/// let json2: ZCString = ZCString::l(literal_substr!("{\"a\": \"b\", \"c\":[1, \"BAD\", 3]}"));
/// let x2: X = serde_json_from_zcstring(json2)?;
/// assert!(x2.c.value().is_none());
///
/// // JSON with missing "c"
/// let json3: ZCString = ZCString::l(literal_substr!("{\"a\": \"b\"}"));
/// let x3: X = serde_json_from_zcstring(json3)?;
/// assert!(x3.c.value().is_none());
/// let x3d = serde_json::to_string(&x3)?;
/// assert_eq!(x3d, "{\"a\":\"b\"}");
///
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "serde_json")]
pub struct ZCLazyValue<T> {
    raw: ZCRawValue,
    // The inner Option<T> is logically redundant but allows return of
    // a clean &Option<T> return
    value: OnceLock<Result<Option<T>, Arc<serde_json::Error>>>,
}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de>> ZCLazyValue<T> {
    // our raw ZCString
    pub fn raw(&self) -> &ZCString {
        self.raw.value()
    }

    // internal call to OnceLock
    fn get_or_init(&self) -> &Result<Option<T>, Arc<serde_json::Error>> {
        self.value
            .get_or_init(|| {
                match serde_json_from_zcstring(self.raw.value().clone()) {
                    Ok(t) => Ok(Some(t)),
                    Err(e) => Err(Arc::new(e)),
                }
            })
    }

    // value as a Result, Ok(t) if successfully decoded
    pub fn try_value(&self) -> Result<&Option<T>, &Arc<serde_json::Error>> {
        self.get_or_init().as_ref()
    }

    // value as an option, Some(t) on successful decode
    pub fn value(&self) -> Option<&T> {
        match self.get_or_init() {
            Ok(t) => t.as_ref(),
            Err(_) => None,
        }
    }

    // return true if the value has been decoded
    pub fn decoded(&self) -> bool {
        self.value.get().is_some()
    }
    
    // return true if the value is defaulted
    pub fn is_default(&self) -> bool {
        match self.value.get() {
            // This is a default construct
            Some(Ok(None)) => true,
            // Not default
            _ => false,
        }
    }
}

#[cfg(feature = "serde_json")]
impl<'de, T> Deserialize<'de> for ZCLazyValue<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: ZCRawValue = 
            Deserialize::deserialize(deserializer)?;
            
        // now deserialize the data component from our raw string
        // raw is a substring of our data source, we don't need
        // to re-specify it.
        Ok(ZCLazyValue {
            raw,
            value: OnceLock::new(),
        })
    }
}

#[cfg(feature = "serde_json")]
impl<T: Serialize> Serialize for ZCLazyValue<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // just send back the raw version
        self.raw.serialize(serializer)
    }
}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de> + Debug> fmt::Debug for ZCLazyValue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.try_value(), f)
    }
}

#[cfg(feature = "serde_json")]
impl<T> Clone for ZCLazyValue<T> {
    fn clone(&self) -> Self {
         match self.value.get() {
             // This is a default construct
             Some(Ok(None)) => Self::default(),
             // Otherwise clone the raw string
             _ => Self {
                raw: self.raw.clone(),
                value: OnceLock::new(),
            }
        }
    }
}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de> + PartialEq> PartialEq for ZCLazyValue<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self.value(), other.value()) {
            // compare decoded contents
            (Some(a), Some(b)) => a.eq(b),
            // treat as None vs None
            _ => true,
        }
    }
}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de> + Eq> Eq for ZCLazyValue<T> {}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de> + PartialOrd> PartialOrd for ZCLazyValue<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self.value(), other.value()) {
            // compare decoded contents
            (Some(a), Some(b)) => a.partial_cmp(b),
            _ => Some(Ordering::Equal),
        }
    }
}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de> + Ord> Ord for ZCLazyValue<T> {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.value(), other.value()) {
            // compare decoded contents
            (Some(a), Some(b)) => a.cmp(b),
            // treat like comparison of None == None
            _ => Ordering::Equal,
        }
    }
}

#[cfg(feature = "serde_json")]
impl<T: for<'de> Deserialize<'de> + Hash> Hash for ZCLazyValue<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self.value() {
            Some(v) => {
                1_u8.hash(state);
                v.hash(state)
            },

            _ => {
                0_u8.hash(state)
            }
        }
    }
}

// default is an errorless None
// To correctly omit the field rather than serializing to null
// use the following
// #[serde(default, skip_serializing_if = "ZCLazyValue::is_default")]
#[cfg(feature = "serde_json")]
impl<T> Default for ZCLazyValue<T> {
    fn default() -> Self {
        let n: ZCString = literal!("null").into();
        Self {
            raw: ZCRawValue(n),
            value: OnceLock::from(Ok(None)),
        }
    }
}
